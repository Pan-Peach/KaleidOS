//! 物理内存与 Core Heap（M1：基于 `buddy_system_allocator::MetadataHeap`）。
//!
//! 设计（经 MangoCore 实践验证，详见 docs/09_debug/buddy-allocator-scan-drift.md）：
//! MetadataHeap 的 per-unit BlockMeta（state=Reserved/Free/Used，O(1) buddy 查询）
//! 本身就是帧真相 —— 不像上游 linked-list 版在 dealloc 时线性扫 free-list。
//!
//! 规则（Core 与组件共享）：
//! - 一个 `MetadataHeap<32, 12>` 实例：`alloc_pages` 提供连续的物理区域；
//!   `alloc(layout)` 提供小对象堆。
//! - 区域 = `[align_up(__bootstrap_end), RAM 末尾)` —— ELF/BSS/DTB 在区域外，
//!   天然保留，无需 reserve API。
//! - 无 per-component 记账：组件与 Core 共享同一 heap；ResourceDomain 只记 handle。
//!
use crate::log;
use crate::memory::address_space::PhysicalRange;
use buddy_system_allocator::{MetadataHeap, PageOrder, PageRun};
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use slab::SlabAllocator;
use spin::Mutex;

pub mod address_space;
pub mod kernel_mappings;
mod slab;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// 物理内存分配粒度（buddy 最小单元 / 区域对齐），当前 4 KiB。
///
/// 这只是**分配器/物理内存机制**的粒度，与 VM 翻译粒度无关
/// （翻译粒度由各 `AddressSpaceBackend` 的 `GRANULE` 声明，见 arch/src/vm.rs）。
/// NoMMU 目标允许该值随 build/profile 变化，不承诺 4 KiB。
pub const ALLOC_GRANULE: usize = 4096;

/// MetadataHeap 最大 order（FREE_AREA 槽数；最大块 = 8B << 31 = 16 TiB）。
pub const HEAP_ORDER: usize = 32;

/// 最小分配单元 = 4 KiB。
pub const HEAP_MIN_ORDER: usize = 12;

/// 对齐到最小物理页。
pub const fn align_up_page(addr: usize) -> usize {
    (addr + ALLOC_GRANULE - 1) & !(ALLOC_GRANULE - 1)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryError {
    Exhausted,
    InvalidSize,
    DoubleFree,
}

// ---------------------------------------------------------------------------
// Region lease
// ---------------------------------------------------------------------------

/// **唯一的 RAII 分配属主**：拥有 `region` 这段物理区域的占用（buddy 块）。
///
/// 它不是"借用 / 引用计数 pin"：没有 clone、没有共享计数，`Drop` 时把区域
/// 整体归还 buddy heap。谁持有它，谁就独占这段物理内存；`forget` 即保活。
/// DMA backing 用 `Option<MemoryLease>` 承载这份占用，回收时 move 进 Core 私有
/// `QUARANTINE`（见 `resource/dma.rs`）。
///
/// 命名对照：它**不是** capability；MMIO/DMA 数据路径上的 old `*View/*Lease`
/// 中间层已随 authority 模型一起删除（见 `resource` 模块文档）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MemoryLease {
    region: PhysicalRange,
    order: usize,
}

impl MemoryLease {
    pub(crate) const fn region(&self) -> PhysicalRange {
        self.region
    }

    /// 区域物理基址（DMA 等 Core 内部派生路径用；不对组件暴露）。
    pub(crate) const fn base(&self) -> usize {
        self.region.base
    }

    /// 区域实际容量（buddy order 的块大小，≥ 请求尺寸）。
    pub(crate) const fn size(&self) -> usize {
        self.region.size
    }
}

impl Drop for MemoryLease {
    fn drop(&mut self) {
        let _ = release_region(self.region, self.order);
    }
}

// ---------------------------------------------------------------------------
// The single heap instance (frame + small objects)
// ---------------------------------------------------------------------------

/// Core 唯一的物理内存机制：区域分配 + 小对象堆。
/// Phase 1 单核（副 hart 已 park），spin 锁够用；
/// 多核唤醒后需评估锁粒度。
static HEAP: Mutex<MetadataHeap<HEAP_ORDER, HEAP_MIN_ORDER>> = Mutex::new(MetadataHeap::empty());

static SLABS: Mutex<SlabAllocator> = Mutex::new(SlabAllocator::new());

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

/// Core 物理内存初始化：
/// - `region_start`：物理区域起点（bootstrap 传页对齐后的 image 末尾）；
///   该点之前（ELF/BSS/DTB）天然保留。
/// - `frame_end`：RAM 区域末尾。
///
/// MetadataHeap 的 metadata 从区域前端 carve，自我描述（无鸡生蛋）。
pub fn init(region_start: usize, region_end: usize) -> Result<(), &'static str> {
    if region_start >= region_end {
        return Err("invalid frame region");
    }

    // try_init 不安全：调用方保证区间有效、未被他方管理。
    let init_result = {
        let mut heap = HEAP.lock();
        unsafe { heap.try_init(region_start, region_end - region_start) }
            .map_err(|_| "frame region init failed")
    };

    // 锁后日志：不持分配器锁打印（打印可能分配/被 panic 中途打断）。
    match &init_result {
        Ok(()) => {
            let span_pages = (region_end - region_start) / ALLOC_GRANULE;
            log!(
                "memory",
                "region 0x{:x}-0x{:x} span_pages={}",
                region_start,
                region_end,
                span_pages
            );
            log!("memory", "init OK");
        }
        Err(e) => {
            log!("memory", "init FAILED: {}", e);
        }
    }
    init_result
}

// ---------------------------------------------------------------------------
// Region API（Core canonical 入口）
// ---------------------------------------------------------------------------

fn order_for_size(size: usize) -> Result<usize, MemoryError> {
    if size == 0 {
        return Err(MemoryError::InvalidSize);
    }
    let mut order = HEAP_MIN_ORDER;
    let mut capacity = ALLOC_GRANULE;
    while capacity < size {
        capacity = capacity.checked_mul(2).ok_or(MemoryError::InvalidSize)?;
        order += 1;
        if order >= HEAP_ORDER {
            return Err(MemoryError::InvalidSize);
        }
    }
    Ok(order)
}

/// 分配一段连续物理区域。实际分配大小是 buddy order 的容量。
pub(crate) fn alloc_region(size: usize) -> Result<MemoryLease, MemoryError> {
    let order = order_for_size(size)?;
    let mut heap = HEAP.lock();
    let run = heap
        .alloc_pages(PageOrder(order as u8))
        .map_err(|_| MemoryError::Exhausted)?;
    Ok(MemoryLease {
        region: PhysicalRange {
            base: run.base.as_ptr() as usize,
            size: 1usize << order,
        },
        order,
    })
}

/// 释放一次区域分配。lease 被消费后不能重复释放。
pub(crate) fn free_region(lease: MemoryLease) -> Result<(), MemoryError> {
    let region = lease.region;
    let order = lease.order;
    core::mem::forget(lease);
    release_region(region, order)
}

/// 释放一段**只有 `(base, size)` 身份**的区域（无 lease 的显式 release 路径）。
///
/// 契约 `kcore_memory_acquire/release` 无 Core 侧账本：`view` 自身就是身份，
/// 因此释放端只能按调用方交回的 `(base, size)` 归还——本函数只接受与
/// [`alloc_region`] 产物同形的参数（`base` 非零且页对齐；`size` 恰好落在某个
/// buddy order 上，即 ≥ 一页的 2 的幂），其它一律 `InvalidSize`：**绝不猜测
/// 块大小去释放**（错配 = 归还一段不属于自己的物理内存）。
pub(crate) fn free_region_raw(base: usize, size: usize) -> Result<(), MemoryError> {
    if base == 0 || !base.is_multiple_of(ALLOC_GRANULE) || !size.is_power_of_two() {
        return Err(MemoryError::InvalidSize);
    }
    let order = order_for_size(size)?;
    if (1usize << order) != size {
        return Err(MemoryError::InvalidSize);
    }
    release_region(PhysicalRange { base, size }, order)
}

fn release_region(region: PhysicalRange, order: usize) -> Result<(), MemoryError> {
    let mut heap = HEAP.lock();
    let base = region.base as *mut u8;
    let run = PageRun {
        base: unsafe { NonNull::new_unchecked(base) },
        order: PageOrder(order as u8),
    };
    unsafe { heap.dealloc_pages(run) };
    Ok(())
}

// ---------------------------------------------------------------------------
// 页表页取页函数（供 RISC-V translation backend 通过函数指针调用 buddy heap）
// ---------------------------------------------------------------------------

/// 从 buddy heap 取一个已归零的页，返回其物理地址。
///
/// 这是 arch 页表 backend 用的 `PageAlloc`：core 在 init 后把它塞进页表。
/// v1 仍处 identity 阶段，物理地址可当虚拟地址解引用（pa == va）；
/// 切纯高半区后需在返回值基础上换算成可写的 `KernelVirtualAddress`。
///
/// 注意：`alloc_region` 不做清零，必须手动 `write_bytes` 归零，
/// 否则新页表页会继承旧内存里的垃圾（被当成 PTE 就出大问题）。
// The `()` error is part of arch::vm::PageAlloc's deliberately narrow
// cross-crate callback contract; allocation diagnostics stay in Core.
#[allow(clippy::result_unit_err)]
pub fn vm_page_alloc() -> Result<usize, ()> {
    let lease = alloc_region(ALLOC_GRANULE).map_err(|_| ())?;
    let base = lease.region().base;
    // 页表页在 space 销毁前不归还（v1 无回收），因此 forget lease 保活。
    core::mem::forget(lease);
    // SAFETY: base 来自 alloc_region，已页对齐且是 4K 有效物理页，可写。
    unsafe {
        core::ptr::write_bytes(base as *mut u8, 0, ALLOC_GRANULE);
    }
    Ok(base)
}

// ---------------------------------------------------------------------------
// GlobalAlloc：小对象堆（组件与 Core 共享）
// ---------------------------------------------------------------------------

pub struct KernelAllocator;

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match slab::slab_class(layout) {
            Some(class_size) => SLABS
                .lock()
                .alloc(class_size)
                .map(|nn| nn.as_ptr())
                .unwrap_or(core::ptr::null_mut()),
            None => HEAP
                .lock()
                .alloc(layout)
                .map(|nn| nn.as_ptr())
                .unwrap_or(core::ptr::null_mut()),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr/layout 必须匹配一次成功的 alloc（Rust 调用方保证）。
        match slab::slab_class(layout) {
            Some(class_size) => {
                SLABS.lock().dealloc(ptr, class_size);
            }
            None => unsafe {
                HEAP.lock().dealloc(NonNull::new_unchecked(ptr), layout);
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// 空闲帧块分布（按 order 直方图；调试/检查用）。
pub fn free_block_counts() -> [usize; HEAP_ORDER] {
    HEAP.lock().free_block_counts()
}

// ---------------------------------------------------------------------------
// Tests（host，借用 std 内存做 backing）
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_region_alloc_and_free() {
        let _g = test_support::GUARD.lock();
        test_support::ensure_init();

        let lease = alloc_region(ALLOC_GRANULE).expect("alloc should succeed");
        let region = lease.region();
        assert_eq!(region.base % ALLOC_GRANULE, 0);
        free_region(lease).expect("free should succeed");
        let second = alloc_region(ALLOC_GRANULE).expect("alloc after free should succeed");
        assert_eq!(region, second.region());
        free_region(second).expect("free second");
        assert_eq!(region.size, ALLOC_GRANULE);
    }

    #[test]
    fn alloc_after_free_reuses() {
        let _g = test_support::GUARD.lock();
        test_support::ensure_init();

        let first = alloc_region(ALLOC_GRANULE).expect("alloc first");
        let first_region = first.region();
        free_region(first).expect("free first");
        let second = alloc_region(ALLOC_GRANULE).expect("alloc second");
        assert_eq!(
            first_region,
            second.region(),
            "buddy should reuse the freed block"
        );
        free_region(second).expect("free second");
    }

    #[test]
    fn kernel_allocator_uses_class_size_for_small_layout() {
        let _g = test_support::GUARD.lock();
        test_support::ensure_init();

        // 24 字节请求应该进入 32-byte slab class，而不是把 24
        // 直接传给 SlabAllocator::alloc。
        let layout = Layout::from_size_align(24, 8).unwrap();
        let ptr = unsafe { KernelAllocator.alloc(layout) };

        assert!(!ptr.is_null(), "small allocation should use the slab");
        assert_eq!(ptr as usize % layout.align(), 0);

        unsafe {
            KernelAllocator.dealloc(ptr, layout);
        }
    }

    // ------------------------------------------------------------------
    // 以下用局部 MetadataHeap 实例验证库行为（不碰全局 HEAP，无污染）
    // ------------------------------------------------------------------

    #[test]
    fn library_exhausts_cleanly() {
        let mut buf = std::vec![0u8; 1 << 20]; // 1 MiB = 256 帧
        let base = buf.as_mut_ptr() as usize;
        let start = (base + ALLOC_GRANULE - 1) & !(ALLOC_GRANULE - 1);
        let mut heap = MetadataHeap::<HEAP_ORDER, HEAP_MIN_ORDER>::empty();
        unsafe { heap.try_init(start, 1 << 20).expect("init") };

        let mut n = 0u32;
        while let Ok(run) = heap.alloc_pages(PageOrder(HEAP_MIN_ORDER as u8)) {
            n += 1;
            assert!(n <= 1024, "runaway alloc");
            // 释放一半，验证合并后再分配（避免残留）
            if n.is_multiple_of(2) {
                unsafe { heap.dealloc_pages(run) };
            }
        }
        assert!(n > 0, "should allocate some");
        // 耗尽后必须报 NoMemory（而非 panic）
        assert!(matches!(
            heap.alloc_pages(PageOrder(HEAP_MIN_ORDER as u8)),
            Err(buddy_system_allocator::AllocError::NoMemory)
        ));
    }

    /// 性能基线：按 order 分档测 alloc/free 往返（`make bench`）。
    ///
    /// 分档而不是只报一个数：buddy 的成本随 order 变化（分裂/合并的层数），
    /// 混成一个数字就看不出来了。以后再加 fragmentation 基准。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_alloc_free_by_order() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();

        crate::bench::report_environment();
        for (name, order) in [
            ("alloc_free.order0", 0u32),
            ("alloc_free.order1", 1),
            ("alloc_free.order2", 2),
            ("alloc_free.order3", 3),
        ] {
            let size = ALLOC_GRANULE << order;
            let mut bench = crate::bench::Bench::new(name);
            bench.run(100, || {
                let lease = alloc_region(size).expect("alloc");
                free_region(lease).expect("free");
            });
            bench.finish().report();
        }
    }

    // ------------------------------------------------------------------
    // init 的非法区间（纯逻辑，返回前不锁 HEAP，不碰全局堆）----
    // ------------------------------------------------------------------

    /// 验收：`region_start >= region_end` 直接被拒，且在**锁 HEAP 之前**返回。
    ///
    /// 测试显式持有 `HEAP` 锁再调用：若 `init` 试图上锁会自旋死锁（测试挂死），
    /// 以此证明非法路径不触碰全局堆——也正因如此，它不会重初始化被其它测试
    /// 共享的 HEAP。`HEAP` 锁下无嵌套 `GUARD` 需求，故不取 GUARD。
    #[test]
    fn init_rejects_invalid_region_without_touching_heap() {
        let _heap = HEAP.lock();
        assert_eq!(init(10, 5), Err("invalid frame region"));
        assert_eq!(init(4096, 4096), Err("invalid frame region"));
    }

    // ------------------------------------------------------------------
    // 纯 helper：order_for_size / align_up_page（无全局状态，不取 GUARD）----
    // ------------------------------------------------------------------

    /// 验收：size → buddy order 的映射 + 越界尺寸拒绝。
    #[test]
    fn order_for_size_maps_sizes_to_buddy_orders() {
        assert_eq!(order_for_size(0), Err(MemoryError::InvalidSize));
        assert_eq!(order_for_size(ALLOC_GRANULE), Ok(HEAP_MIN_ORDER));
        assert_eq!(order_for_size(2 * ALLOC_GRANULE), Ok(HEAP_MIN_ORDER + 1));
        // 非 2 的幂向上取整到下一档
        assert_eq!(order_for_size(ALLOC_GRANULE + 1), Ok(HEAP_MIN_ORDER + 1));
        // 超过最大块 / 溢出 → InvalidSize（不 panic）
        assert_eq!(order_for_size(usize::MAX), Err(MemoryError::InvalidSize));
    }

    /// 验收：`align_up_page` 对已对齐地址不变、对未对齐地址向上取整，
    /// 结果恒为 4 KiB 对齐且 `>= addr`。
    #[test]
    fn align_up_page_rounds_up_and_preserves_alignment() {
        assert_eq!(align_up_page(0x1000), 0x1000, "already aligned stays");
        assert_eq!(align_up_page(0x1001), 0x2000, "unaligned rounds up");

        for addr in [
            0usize,
            1,
            ALLOC_GRANULE - 1,
            ALLOC_GRANULE,
            ALLOC_GRANULE + 1,
        ] {
            let aligned = align_up_page(addr);
            assert!(
                aligned >= addr,
                "align_up_page must never lower the address"
            );
            assert_eq!(aligned % ALLOC_GRANULE, 0, "result must be page-aligned");
            assert!(
                aligned - addr < ALLOC_GRANULE,
                "round-up must advance by less than one granule"
            );
        }
    }

    // ------------------------------------------------------------------
    // 全局 HEAP 入口（分配 / 查询 / lease）——须持 GUARD + ensure_init ----
    // ------------------------------------------------------------------

    /// 验收：`alloc_region(0)` 在锁 HEAP 之前就因非法尺寸返回（无 GUARD 需求）。
    #[test]
    fn alloc_region_rejects_zero_size() {
        assert_eq!(alloc_region(0), Err(MemoryError::InvalidSize));
    }

    /// 验收：`vm_page_alloc()` 返回非空、页对齐、前 16 字节已归零的页。
    ///
    /// 注意：该函数**故意 `forget` lease**（v1 页表页不回收），本测试只调用一次，
    /// 泄漏一页可接受。
    #[test]
    fn vm_page_alloc_returns_zeroed_aligned_page() {
        let _g = test_support::GUARD.lock();
        test_support::ensure_init();

        let page = vm_page_alloc().expect("page allocation should succeed");
        assert_ne!(page, 0, "physical page address must be non-null");
        assert_eq!(page % ALLOC_GRANULE, 0, "page must be granule-aligned");

        // SAFETY: page 来自 alloc_region，是 4 KiB 有效可写物理页（v1 pa==va）。
        let head = unsafe { core::slice::from_raw_parts(page as *const u8, 16) };
        assert!(
            head.iter().all(|&b| b == 0),
            "fresh page must be zeroed in its first 16 bytes"
        );
    }

    /// 验收：`init` 之后 `free_block_counts()` 返回按 order 的直方图，
    /// 且至少有一个 order 存在空闲块（不断言精确数量）。
    #[test]
    fn free_block_counts_reports_nonempty_histogram_after_init() {
        let _g = test_support::GUARD.lock();
        test_support::ensure_init();

        let counts = free_block_counts();
        assert_eq!(
            counts.len(),
            HEAP_ORDER,
            "histogram covers every buddy order"
        );
        let total: usize = counts.iter().sum();
        assert!(
            total > 0,
            "initialized heap must expose at least one free block"
        );
    }

    /// 验收：`MemoryLease` 的 `base()`/`size()` 如实报告底层区域——基址页对齐、
    /// 容量不小于请求尺寸（实为 buddy order 的块大小）；`free_region` 可归还。
    #[test]
    fn memory_lease_accessors_report_region_truth() {
        let _g = test_support::GUARD.lock();
        test_support::ensure_init();

        let lease = alloc_region(ALLOC_GRANULE).expect("alloc should succeed");
        assert!(
            lease.size() >= ALLOC_GRANULE,
            "lease size must cover the requested bytes"
        );
        assert!(
            lease.size().is_power_of_two(),
            "buddy block size is a power of two"
        );
        assert_eq!(lease.base() % ALLOC_GRANULE, 0, "base must be page-aligned");

        free_region(lease).expect("free should succeed");
    }

    // ------------------------------------------------------------------
    // Property：order_for_size 覆盖 + 单调，align_up_page 只向上且 < 一页 ----
    // ------------------------------------------------------------------

    use proptest::prelude::*;

    proptest! {
        /// size 在 1..=(1 MiB) 内必落在合法 order 且能覆盖请求；对采样的一对
        /// (size, other) 单调非减。align_up_page 只向上对齐、步长 < 一页。
        #[test]
        fn size_and_address_helpers_hold_invariants(
            size in 1usize..=(1 << 20),
            other in 1usize..=(1 << 20),
            addr in 0usize..=(1 << 24),
        ) {
            let order = order_for_size(size)
                .expect("every size within 1 MiB must map to an order");
            prop_assert!(order >= HEAP_MIN_ORDER);
            prop_assert!(order < HEAP_ORDER);
            prop_assert!((1usize << order) >= size, "order must cover the request");

            // 单调非减：更大的 size 不会得到更小的 order。
            let other_order = order_for_size(other).expect("in-range size");
            if size <= other {
                prop_assert!(order <= other_order);
            } else {
                prop_assert!(order >= other_order);
            }

            let aligned = align_up_page(addr);
            prop_assert!(aligned >= addr);
            prop_assert_eq!(aligned % ALLOC_GRANULE, 0);
            prop_assert!(aligned - addr < ALLOC_GRANULE);
        }
    }
}
