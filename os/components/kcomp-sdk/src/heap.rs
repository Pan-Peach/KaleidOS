//! per-instance runtime heap（`docs/architecture/memory-and-heap.md` §5 / §6）。
//!
//! 分配器实现是 **freestanding C**（`c/kalloc.c` + `include/kcomp_kalloc.h`），
//! 一份源码私有链进每个 `.kcomp`；这里只是 Rust 侧最小 facade：
//!
//! - [`Heap`]：**不透明句柄**（`[u8; 0]`，不镜像 C 布局——C 布局是私有实现）；
//! - [`Backing`]：与 `kcore_memory_acquire` 同形的 backing 回调；
//! - [`set_current_heap`] / [`current_heap`]：per-instance 解析器占位。
//!
//! # 为什么是 C
//!
//! 契约 §6：分配器是"共享分配器**实现代码**，不是共享堆"。C 组件无法链接
//! Rust，所以真相在 C；Rust `GlobalAlloc`（[`crate::alloc`]）只是 adapter。
//!
//! # v1 边界（明确记录）
//!
//! - **IRQ 上下文分配不支持**：当前实现无锁，在中断里分配会自死锁。
//! - **整段 region release 不在范围内**：v1 不记 region 列表（Core 无账本，
//!   契约 §4）。已接受的副作用：两段独立 `acquire` 的 region 若地址恰好相邻，
//!   free list 上的空闲块可能跨 region 合并。
//! - [`set_current_heap`] 是**占位**：真正的 per-instance runtime slot（`tp`，
//!   契约 §5 / D2）落地前由调用方显式设置；null 堆 → 分配返回 null，
//!   **绝不 panic**。
//!
//! # 句柄是"本执行域访问窗口"
//!
//! [`Heap::place`] 收到的 `base` 就是 Core 交付的本域 VA（与
//! `kcore_device_claim` 的 MMIO 窗口同形）。Core 不记 heap 账；`HeapState`
//! 的内部账完全归本模块 + C 分配器。

use core::sync::atomic::{AtomicPtr, Ordering};

/// 不透明堆句柄：C 分配器的状态在 [`Heap::place`] 的 region 内，Rust 侧不解释。
#[repr(C)]
pub struct Heap {
    _p: [u8; 0],
}

/// backing 回调（C ABI，与 `kcore_memory_acquire` 1:1）。
///
/// 成功 = `0` 且写 `out_base` / `out_len`（`out_len >= min_len`）；失败 =
/// `-Errno` 且不改 out。Rust 调用方通常包一层
/// [`kcore_memory_acquire`](crate::abi::kcore_memory_acquire)。
pub type Backing = unsafe extern "C" fn(
    min_len: usize,
    min_align: usize,
    out_base: *mut usize,
    out_len: *mut usize,
) -> i32;

unsafe extern "C" {
    fn kcomp_heap_place(base: *mut u8, len: usize, backing: Backing) -> *mut Heap;
    fn kcomp_heap_alloc(heap: *mut Heap, size: usize, align: usize) -> *mut u8;
    fn kcomp_heap_free(heap: *mut Heap, ptr: *mut u8);
    fn kcomp_heap_realloc(heap: *mut Heap, ptr: *mut u8, size: usize, align: usize) -> *mut u8;
}

impl Heap {
    /// 把堆状态放进 `base` 处的 region（`len` 字节），返回句柄（正常 == `base`，
    /// Core 的 region 至少页对齐）或 null（参数为空 / region 太小）。
    ///
    /// **不分配**：堆头与首个空闲块都写在这段 region 内。之后
    /// [`alloc`](Heap::alloc) 只在 free list 放不下时才经 `backing` 向 Core
    /// 取新 backing（几何式请求容量）。
    ///
    /// # Safety
    ///
    /// `base..base+len` 必须是本执行域可读写、且不与其它堆 / 对象重叠的一段
    /// 内存；堆存活期间不得被别处使用或 release。
    pub unsafe fn place(base: *mut u8, len: usize, backing: Backing) -> *mut Heap {
        // SAFETY: 由调用方保证 base/len/backing 的契约（见 # Safety）。
        unsafe { kcomp_heap_place(base, len, backing) }
    }

    /// 分配 `size` 字节、`align` 对齐；失败（OOM / 溢出 / 非法 align）返回 null。
    ///
    /// # Safety
    ///
    /// `self` 必须是 [`Heap::place`] 返回且仍有效的句柄。
    pub unsafe fn alloc(&self, size: usize, align: usize) -> *mut u8 {
        // SAFETY: 句柄来自 Heap::place；C 侧只改 region 内的分配器状态。
        unsafe { kcomp_heap_alloc(self as *const Heap as *mut Heap, size, align) }
    }

    /// 归还一次成功分配（null 安全，无动作）。
    ///
    /// # Safety
    ///
    /// `ptr` 必须来自本堆的一次成功分配且尚未释放（GlobalAlloc 契约）。
    pub unsafe fn free(&self, ptr: *mut u8) {
        // SAFETY: 句柄与 ptr 的契约由调用方保证（见 # Safety）。
        unsafe { kcomp_heap_free(self as *const Heap as *mut Heap, ptr) }
    }

    /// 调整大小：成功返回可用指针（可能原地）；失败返回 null 且**旧块原样保留**。
    ///
    /// # Safety
    ///
    /// `ptr` 必须来自本堆的一次成功分配且尚未释放。
    pub unsafe fn realloc(&self, ptr: *mut u8, size: usize, align: usize) -> *mut u8 {
        // SAFETY: 句柄与 ptr 的契约由调用方保证（见 # Safety）。
        unsafe { kcomp_heap_realloc(self as *const Heap as *mut Heap, ptr, size, align) }
    }
}

/// 当前执行域的堆句柄（null = 未设置）。
static CURRENT_HEAP: AtomicPtr<Heap> = AtomicPtr::new(core::ptr::null_mut());

/// 设置当前执行域的堆句柄（null = 无堆；此时分配返回 null，绝不 panic）。
///
/// **占位**：真正的 per-instance runtime slot（`tp`，契约 §5 / D2）落地后由
/// 运行时在组件入口 / 出口自动 save/restore；现在由 bootstrap / 调用方显式设置。
/// 切换只影响本 adapter 的分配归属，**不构成访问强制**（协作式记账，不是鉴权
/// 隔离），也不改变 panic 归属哪个 containment 边界。
pub fn set_current_heap(heap: *mut Heap) {
    CURRENT_HEAP.store(heap, Ordering::Release);
}

/// 当前执行域的堆句柄；未设置时为 null。
pub fn current_heap() -> *mut Heap {
    CURRENT_HEAP.load(Ordering::Acquire)
}
