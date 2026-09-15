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
use spin::Mutex;

pub mod address_space;

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
        HEAP.lock()
            .alloc(layout)
            .map_or(core::ptr::null_mut(), |p| p.as_ptr())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr/layout 必须匹配一次成功的 alloc（Rust 调用方保证）。
        unsafe {
            HEAP.lock().dealloc(NonNull::new_unchecked(ptr), layout);
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
}
