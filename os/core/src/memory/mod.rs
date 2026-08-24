//! 物理帧与 Core Heap（M1：基于 `buddy_system_allocator::MetadataHeap`）。
//!
//! 设计（经 MangoCore 实践验证，详见 docs/09_debug/buddy-allocator-scan-drift.md）：
//! MetadataHeap 的 per-unit BlockMeta（state=Reserved/Free/Used，O(1) buddy 查询）
//! 本身就是帧真相 —— 不像上游 linked-list 版在 dealloc 时线性扫 free-list。
//!
//! 规则（Core 与组件共享）：
//! - 一个 `MetadataHeap<32, 12>` 实例：`alloc_pages(PageOrder(12))` = 物理帧
//!   （4K 单元，order 12=4K, 13=8K, ... 23=32M）；`alloc(layout)` = 小对象堆。
//! - 区域 = `[align_up(__bootstrap_end), RAM 末尾)` —— ELF/BSS/DTB 在区域外，
//!   天然保留，无需 reserve API。
//! - 无 per-component 记账：组件与 Core 共享同一 heap；ResourceDomain 只记 handle。
//!
//! FrameId = 帧身份；从 `PageRun.base / FRAME_SIZE` 换算。

use crate::log;
use buddy_system_allocator::{MetadataHeap, PageOrder, PageRun};
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use spin::Mutex;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// 帧大小（4 KiB）。
pub const FRAME_SIZE: usize = 4096;

/// MetadataHeap 最大 order（FREE_AREA 槽数；最大块 = 8B << 31 = 16 TiB）。
pub const HEAP_ORDER: usize = 32;

/// 最小单元 = 4 KiB（帧粒度；alloc_pages 最小 order 12 = 一帧）。
pub const HEAP_MIN_ORDER: usize = 12;

/// 帧分配 order（= HEAP_MIN_ORDER，4K 帧）。
pub const FRAME_ORDER: PageOrder = PageOrder(12);

/// 对齐到帧（向上取整）。
pub const fn align_up_frame(addr: usize) -> usize {
    (addr + FRAME_SIZE - 1) & !(FRAME_SIZE - 1)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    Exhausted,
    InvalidOrder,
    DoubleFree,
}

// ---------------------------------------------------------------------------
// FrameId identity
// ---------------------------------------------------------------------------

/// 物理帧身份（M1 词汇表）—— Identity，不是 Authority。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameId(u64);

impl FrameId {
    pub const fn from_pa(pa: usize) -> Self {
        Self((pa as u64) / (FRAME_SIZE as u64))
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn start_pa(self) -> usize {
        self.0 as usize * FRAME_SIZE
    }
}

// ---------------------------------------------------------------------------
// The single heap instance (frame + small objects)
// ---------------------------------------------------------------------------

/// Core 唯一的物理内存机制：物理帧分配 + 小对象堆。
/// Phase 1 单核（副 hart 已 park），spin 锁够用；
/// 多核唤醒后需评估锁粒度。
static HEAP: Mutex<MetadataHeap<HEAP_ORDER, HEAP_MIN_ORDER>> = Mutex::new(MetadataHeap::empty());

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

/// Core 物理内存初始化：
/// - `frame_start`：帧区域起点（bootstrap 传 `align_up(__bootstrap_end)`）；
///   该点之前（ELF/BSS/DTB）天然保留。
/// - `frame_end`：RAM 区域末尾。
///
/// MetadataHeap 的 metadata 从区域前端 carve，自我描述（无鸡生蛋）。
pub fn init(frame_start: usize, frame_end: usize) -> Result<(), &'static str> {
    if frame_start >= frame_end {
        return Err("invalid frame region");
    }

    // try_init 不安全：调用方保证区间有效、未被他方管理。
    let init_result = {
        let mut heap = HEAP.lock();
        unsafe { heap.try_init(frame_start, frame_end - frame_start) }
            .map_err(|_| "frame region init failed")
    };

    // 锁后日志：不持分配器锁打印（打印可能分配/被 panic 中途打断）。
    match &init_result {
        Ok(()) => {
            let span_frames = (frame_end - frame_start) / FRAME_SIZE;
            log!("memory", "region 0x{:x}-0x{:x} span_frames={}", frame_start, frame_end, span_frames);
            log!("memory", "init OK");
        }
        Err(e) => {
            log!("memory", "init FAILED: {}", e);
        }
    }
    init_result
}

// ---------------------------------------------------------------------------
// Frame API（帧真相 + 帧分配的 canonical 入口）
// ---------------------------------------------------------------------------

/// 分配一帧（4K）。返回帧身份。帧内容未清零（调用方负责）。
pub fn alloc_frame() -> Result<FrameId, FrameError> {
    let mut heap = HEAP.lock();
    let run = heap
        .alloc_pages(FRAME_ORDER)
        .map_err(|_| FrameError::Exhausted)?;
    Ok(FrameId::from_pa(run.base.as_ptr() as usize))
}

/// 释放一帧。`frame` 必须匹配一次 `alloc_frame`（不可重复释放）。
pub fn free_frame(frame: FrameId) -> Result<(), FrameError> {
    let mut heap = HEAP.lock();
    let base = frame.start_pa() as *mut u8;
    let run = PageRun {
        base: unsafe { NonNull::new_unchecked(base) },
        order: FRAME_ORDER,
    };
    // dealloc_pages 内部有 double-free 检测（debug_assert）；错误映射为 DoubleFree。
    unsafe { heap.dealloc_pages(run) };
    Ok(())
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
mod tests {
    use super::*;
    use spin::Mutex;

    /// 全局 HEAP 的公共 API 测试（务必各自分配后释放，不耗尽）。
    static GUARD: Mutex<()> = Mutex::new(());
    static INIT: std::sync::Once = std::sync::Once::new();

    /// 只初始化一次全局 HEAP（try_init 非幂等）；backing 泄漏到进程结束。
    fn ensure_init() {
        INIT.call_once(|| {
            let mut buf = std::vec![0u8; 1 << 21];
            let base = buf.as_mut_ptr() as usize;
            let start = (base + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
            let mut heap = HEAP.lock();
            unsafe { heap.try_init(start, 1 << 21).expect("test init failed") };
            core::mem::forget(buf);
        });
    }

    #[test]
    fn init_frame_alloc_and_free() {
        let _g = GUARD.lock();
        ensure_init();

        let f = alloc_frame().expect("alloc should succeed");
        assert_eq!(f.start_pa() % FRAME_SIZE, 0);
        free_frame(f).expect("free should succeed");
        let f2 = alloc_frame().expect("alloc after free should succeed");
        assert_eq!(f2.start_pa() % FRAME_SIZE, 0);
        free_frame(f2).expect("free f2");
    }

    #[test]
    fn alloc_after_free_reuses() {
        let _g = GUARD.lock();
        ensure_init();

        let f1 = alloc_frame().expect("alloc f1");
        free_frame(f1).expect("free f1");
        let f2 = alloc_frame().expect("alloc f2");
        assert_eq!(f1, f2, "buddy should reuse the freed block");
        free_frame(f2).expect("free f2");
    }

    // ------------------------------------------------------------------
    // 以下用局部 MetadataHeap 实例验证库行为（不碰全局 HEAP，无污染）
    // ------------------------------------------------------------------

    #[test]
    fn library_exhausts_cleanly() {
        let mut buf = std::vec![0u8; 1 << 20]; // 1 MiB = 256 帧
        let base = buf.as_mut_ptr() as usize;
        let start = (base + FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
        let mut heap = MetadataHeap::<HEAP_ORDER, HEAP_MIN_ORDER>::empty();
        unsafe { heap.try_init(start, 1 << 20).expect("init") };

        let mut n = 0u32;
        while let Ok(run) = heap.alloc_pages(FRAME_ORDER) {
            n += 1;
            assert!(n <= 1024, "runaway alloc");
            // 释放一半，验证合并后再分配（避免残留）
            if n % 2 == 0 {
                unsafe { heap.dealloc_pages(run) };
            }
        }
        assert!(n > 0, "should allocate some");
        // 耗尽后必须报 NoMemory（而非 panic）
        assert!(matches!(
            heap.alloc_pages(FRAME_ORDER),
            Err(buddy_system_allocator::AllocError::NoMemory)
        ));
    }
}
