use core::alloc::Layout;
use core::mem::size_of;
use core::ptr::NonNull;

use super::{HEAP, HEAP_MIN_ORDER, PageOrder, PageRun};

const SLAB_PAGE_ORDER: usize = HEAP_MIN_ORDER;
const SLAB_PAGE_SIZE: usize = 1usize << SLAB_PAGE_ORDER;
const MAX_SLAB_CLASS: usize = SLAB_PAGE_SIZE >> 2;
const MIN_CLASS_ORDER: usize = size_of::<usize>().trailing_zeros() as usize;
const MAX_CLASS_ORDER: usize = MAX_SLAB_CLASS.trailing_zeros() as usize;
const CLASS_COUNT: usize = MAX_CLASS_ORDER - MIN_CLASS_ORDER + 1;
const NONE: u16 = u16::MAX;
const PAGE_MAGIC: usize = 0x534C_4142; // "SLAB"
const USED_WORDS: usize = 16;
const MAX_OBJECTS: usize = USED_WORDS * 64;

pub(super) fn slab_class(layout: Layout) -> Option<usize> {
    if layout.size() == 0 {
        return None;
    }

    let required = layout
        .size()
        .max(layout.align())
        .max(core::mem::size_of::<usize>());

    let class = required.checked_next_power_of_two()?;

    if class <= MAX_SLAB_CLASS {
        Some(class)
    } else {
        None
    }
}

fn class_index(class_size: usize) -> Option<usize> {
    if !class_size.is_power_of_two() {
        return None;
    }

    let order = class_size.trailing_zeros() as usize;

    if !(MIN_CLASS_ORDER..=MAX_CLASS_ORDER).contains(&order) {
        None
    } else {
        Some(order - MIN_CLASS_ORDER)
    }
}

fn align_up(value: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    value.checked_add(align - 1).map(|v| v & !(align - 1))
}

#[repr(C)]
struct SlabPage {
    magic: usize,
    class_size: usize,

    /// 第一个 object 的地址
    first_object: usize,
    object_count: u16,
    free_count: u16,
    free_head: u16,

    /// 下一个同 class slab page 的地址（0 表示无）
    next_page: usize,

    // 用于检测 doubel free
    used: [u64; USED_WORDS],
}

impl SlabPage {
    fn is_used(&self, index: usize) -> bool {
        let word = index / 64;
        let bit = index % 64;
        (self.used[word] & (1 << bit)) != 0
    }

    fn mark_used(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.used[word] |= 1 << bit;
    }

    fn mark_free(&mut self, index: usize) {
        let word = index / 64;
        let bit = index % 64;
        self.used[word] &= !(1 << bit);
    }
}
pub(super) struct SlabAllocator {
    /// 每个 class 的 slab page 链表头（0 表示无）
    class_heads: [usize; CLASS_COUNT],
}

impl SlabAllocator {
    /// Read existing pages under the allocator lock; no side counters or
    /// allocation ledger. Bytes are occupied class slots, not requested sizes.
    pub(super) fn stats(&self) -> (usize, usize, usize) {
        let (mut pages, mut objects, mut bytes) = (0, 0, 0);
        for &head in &self.class_heads {
            let mut base = head;
            while base != 0 {
                let page = unsafe { &*(base as *const SlabPage) };
                let used = usize::from(page.object_count - page.free_count);
                pages += 1;
                objects += used;
                bytes += used * page.class_size;
                base = page.next_page;
            }
        }
        (pages, objects, bytes)
    }

    pub(super) const fn new() -> Self {
        Self {
            class_heads: [0; CLASS_COUNT],
        }
    }

    pub(super) fn alloc(&mut self, class_size: usize) -> Option<NonNull<u8>> {
        let class = class_index(class_size)?;

        let mut page_base = self.class_heads[class];

        while page_base != 0 {
            let page = unsafe { &mut *(page_base as *mut SlabPage) };

            debug_assert_eq!(page.magic, PAGE_MAGIC);
            debug_assert_eq!(page.class_size, class_size);

            if page.free_count != 0 {
                return Some(Self::alloc_from_page(page));
            }

            page_base = page.next_page;
        }

        // 向buddy请求一个新的 slab page
        let new_page = Self::new_page(class_size, self.class_heads[class])?;
        self.class_heads[class] = new_page;
        let page = unsafe { &mut *(new_page as *mut SlabPage) };
        Some(Self::alloc_from_page(page))
    }

    pub(super) fn dealloc(&mut self, ptr: *mut u8, class_size: usize) {
        let class = class_index(class_size).expect("invalid class size");
        let page_base = (ptr as usize) & !(SLAB_PAGE_SIZE - 1);
        let page = unsafe { &mut *(page_base as *mut SlabPage) };

        debug_assert_eq!(page.magic, PAGE_MAGIC);
        debug_assert_eq!(page.class_size, class_size);

        let index = (ptr as usize - page.first_object) / class_size;

        debug_assert!(index < page.object_count as usize);
        debug_assert!(page.is_used(index));

        let old_head = page.free_head;

        unsafe {
            (ptr as *mut u16).write(old_head);
        }

        page.free_head = index as u16;
        page.mark_free(index);
        page.free_count += 1;

        if page.free_count == page.object_count {
            // slab page 空了，释放给 buddy
            self.release_empty_page_if_possible(class, page_base);
        }
    }

    fn alloc_from_page(page: &mut SlabPage) -> NonNull<u8> {
        debug_assert_ne!(page.free_count, 0);

        let index = page.free_head as usize;
        let object = page.first_object + index * page.class_size;
        let next = unsafe { *(object as *const u16) };

        page.free_head = next;
        page.free_count -= 1;
        page.mark_used(index);

        unsafe { NonNull::new_unchecked(object as *mut u8) }
    }

    fn new_page(class_size: usize, next_page: usize) -> Option<usize> {
        let first_offset = align_up(core::mem::size_of::<SlabPage>(), class_size)?;

        if first_offset >= SLAB_PAGE_SIZE {
            return None;
        }

        let object_count = (SLAB_PAGE_SIZE - first_offset) / class_size;

        if object_count == 0 || object_count > MAX_OBJECTS {
            return None;
        }

        let run = {
            let mut heap = super::HEAP.lock();

            match heap.alloc_pages(PageOrder(SLAB_PAGE_ORDER as u8)) {
                Ok(page) => page,
                Err(_) => return None,
            }
        };

        let base = run.base.as_ptr() as usize;

        let Some(first_object) = base.checked_add(first_offset) else {
            Self::release_run(run);
            return None;
        };

        let page = SlabPage {
            magic: PAGE_MAGIC,
            class_size,
            first_object,
            object_count: object_count as u16,
            free_count: object_count as u16,
            free_head: 0,
            next_page,
            used: [0; USED_WORDS],
        };

        unsafe {
            (base as *mut SlabPage).write(page);

            for index in 0..object_count {
                let object = first_object + index * class_size;

                let next = if index + 1 < object_count {
                    (index + 1) as u16
                } else {
                    NONE
                };

                (object as *mut u16).write(next);
            }
        }

        Some(base)
    }

    fn release_empty_page_if_possible(&mut self, class: usize, target: usize) {
        let mut prev_page_base = 0;
        let mut page_base = self.class_heads[class];

        while page_base != 0 {
            let page = unsafe { &mut *(page_base as *mut SlabPage) };

            if page_base == target {
                debug_assert_eq!(page.free_count, page.object_count);

                if prev_page_base == 0 {
                    self.class_heads[class] = page.next_page;
                } else {
                    let prev_page = unsafe { &mut *(prev_page_base as *mut SlabPage) };
                    prev_page.next_page = page.next_page;
                }

                Self::release_run(PageRun {
                    base: NonNull::new(page_base as *mut u8).unwrap(),
                    order: PageOrder(SLAB_PAGE_ORDER as u8),
                });

                return;
            }

            prev_page_base = page_base;
            page_base = page.next_page;
        }
    }

    fn release_run(run: PageRun) {
        let mut heap = HEAP.lock();
        unsafe { heap.dealloc_pages(run) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::test_support;

    fn setup() -> test_support::Guard<'static> {
        let guard = test_support::GUARD.lock();
        test_support::ensure_init();
        guard
    }

    #[test]
    fn class_selection_rounds_size_and_alignment() {
        let word = core::mem::size_of::<usize>();

        assert_eq!(
            slab_class(Layout::from_size_align(1, 1).unwrap()),
            Some(word)
        );
        assert_eq!(
            slab_class(Layout::from_size_align(24, 8).unwrap()),
            Some(32)
        );
        assert_eq!(
            slab_class(Layout::from_size_align(9, 64).unwrap()),
            Some(64)
        );
        assert_eq!(
            slab_class(Layout::from_size_align(MAX_SLAB_CLASS + 1, 1).unwrap()),
            None
        );
        assert_eq!(slab_class(Layout::from_size_align(0, 1).unwrap()), None);
    }

    #[test]
    fn class_index_only_accepts_supported_power_of_two_classes() {
        let min_class = 1usize << MIN_CLASS_ORDER;

        assert_eq!(class_index(min_class), Some(0));
        assert_eq!(class_index(MAX_SLAB_CLASS), Some(CLASS_COUNT - 1));
        assert_eq!(class_index(24), None);
        assert_eq!(class_index(MAX_SLAB_CLASS * 2), None);
    }

    #[test]
    fn new_page_initializes_header_and_free_list() {
        let _guard = setup();
        let class_size = 32;
        let mut slab = SlabAllocator::new();

        let ptr = slab.alloc(class_size).expect("first slab allocation");
        let page_base = (ptr.as_ptr() as usize) & !(SLAB_PAGE_SIZE - 1);
        let page = unsafe { &*(page_base as *const SlabPage) };

        assert_eq!(page.magic, PAGE_MAGIC);
        assert_eq!(page.class_size, class_size);
        assert!(page.first_object >= page_base + size_of::<SlabPage>());
        assert_eq!(page.first_object % class_size, 0);
        assert!(page.object_count > 1);
        assert_eq!(page.free_count, page.object_count - 1);
        assert_eq!(page.free_head, 1);
        assert!(page.is_used(0));
        assert!(!page.is_used(1));

        slab.dealloc(ptr.as_ptr(), class_size);
    }

    #[test]
    fn allocation_returns_distinct_aligned_objects() {
        let _guard = setup();
        let class_size = 64;
        let mut slab = SlabAllocator::new();

        let first = slab.alloc(class_size).expect("first allocation");
        let second = slab.alloc(class_size).expect("second allocation");

        assert_ne!(first, second);
        assert_eq!(first.as_ptr() as usize % class_size, 0);
        assert_eq!(second.as_ptr() as usize % class_size, 0);

        slab.dealloc(first.as_ptr(), class_size);
        slab.dealloc(second.as_ptr(), class_size);
    }

    #[test]
    fn dealloc_recycles_the_same_object() {
        let _guard = setup();
        let class_size = 32;
        let mut slab = SlabAllocator::new();

        let first = slab.alloc(class_size).expect("first allocation");
        let second = slab.alloc(class_size).expect("second allocation");

        slab.dealloc(first.as_ptr(), class_size);
        let recycled = slab.alloc(class_size).expect("recycled allocation");

        // 这个断言会检查 dealloc 是否把 object 重新挂回 free_head。
        let recycled_first = recycled == first;

        // 清理当前测试占用的对象，避免失败时污染全局测试 heap。
        slab.dealloc(second.as_ptr(), class_size);
        slab.dealloc(recycled.as_ptr(), class_size);

        assert!(recycled_first, "freed object was not returned to free list");
    }

    #[test]
    fn dealloc_restores_free_count_and_bitmap() {
        let _guard = setup();
        let class_size = 32;
        let mut slab = SlabAllocator::new();

        let first = slab.alloc(class_size).expect("first allocation");
        let second = slab.alloc(class_size).expect("second allocation");
        let page_base = (first.as_ptr() as usize) & !(SLAB_PAGE_SIZE - 1);

        let page = unsafe { &*(page_base as *const SlabPage) };
        let total = page.object_count;
        assert_eq!(page.free_count, total - 2);

        slab.dealloc(first.as_ptr(), class_size);

        let page = unsafe { &*(page_base as *const SlabPage) };
        assert_eq!(page.free_count, total - 1);
        assert_eq!(page.free_head, 0);
        assert!(!page.is_used(0));
        assert!(page.is_used(1));

        slab.dealloc(second.as_ptr(), class_size);
    }

    #[test]
    fn invalid_class_is_rejected() {
        let mut slab = SlabAllocator::new();

        assert!(slab.alloc(24).is_none());
        assert!(slab.alloc(MAX_SLAB_CLASS * 2).is_none());
    }

    #[test]
    fn stats_distinguish_objects_from_shared_pages_and_release() {
        let _guard = setup();
        let mut slab = SlabAllocator::new();
        assert_eq!(slab.stats(), (0, 0, 0));
        let first = slab.alloc(32).unwrap();
        let second = slab.alloc(32).unwrap();
        let other = slab.alloc(64).unwrap();
        assert_eq!(slab.stats(), (2, 3, 128));
        assert_eq!(slab.stats(), (2, 3, 128), "observation has no side effects");
        slab.dealloc(first.as_ptr(), 32);
        assert_eq!(slab.stats(), (2, 2, 96));
        slab.dealloc(second.as_ptr(), 32);
        assert_eq!(slab.stats(), (1, 1, 64));
        slab.dealloc(other.as_ptr(), 64);
        assert_eq!(slab.stats(), (0, 0, 0));
    }
}
