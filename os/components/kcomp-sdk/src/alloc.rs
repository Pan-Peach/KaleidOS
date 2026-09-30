//! alloc adapter：`GlobalAlloc` → **KernelNative 共享堆**
//!（Core `kcore_heap_alloc` / `kcore_heap_dealloc`）。
//!
//! KernelNative 组件与 Core 同特权、同地址空间，因此共享同一个 Core 堆——这是
//! **部署形态决定的窄后端**，不是通用 / 跨域内存 ABI（契约见
//! `docs/architecture/memory-and-heap.md` §6）。Isolated / Sandboxed 不解析这两个
//! 符号；它们的运行时用私有分配器（[`crate::heap`] 的 freestanding C 实现，
//! backing 经 `kcore_memory_acquire/release` 取）。
//!
//! 契约 = Rust `GlobalAlloc`：`dealloc` 的 `(ptr, size, align)` 必须与那次成功
//! alloc **逐字一致**（共享堆按 `Layout` 路由 slab / buddy，不得从取整容量反推）；
//! `realloc` = alloc + copy + dealloc（旧 Layout 原样传给 Core）。Core 侧对非法
//! layout / 耗尽返回 null，adapter 不 panic。
//!
//! `#[global_allocator]` 只在裸机 + feature `alloc` 下注册；host `cargo test`
//! 编译 adapter 类型（测试直接调用它，走 `test_support` 的 Core 替身），但用 std
//! 自己的分配器。

use crate::abi::{kcore_heap_alloc, kcore_heap_dealloc};
use core::alloc::{GlobalAlloc, Layout};

/// 薄 adapter：不拥有内存，只把 `GlobalAlloc` 路由到 Core 的共享堆。
pub struct KernelHeap;

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: layout 由 GlobalAlloc 契约保证 size > 0、align 为 2 的幂；
        // Core 侧仍做 checked Layout 构造，非法输入返回 null。
        unsafe { kcore_heap_alloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr/layout 必须匹配一次成功 alloc（GlobalAlloc 契约）；Core 侧按
        // 原始 Layout 归还（slab / buddy 路由由 Layout 驱动）。
        unsafe { kcore_heap_dealloc(ptr, layout.size(), layout.align()) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // GlobalAlloc 契约：new_size > 0 且对齐后不溢出 isize::MAX，因此
        // from_size_align 不会失败；仍显式处理（不 unwrap、不 panic）。
        let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
            return core::ptr::null_mut();
        };
        // 分配失败 → 返回 null 且旧块原样保留（GlobalAlloc 契约）。
        let new_ptr = unsafe { self.alloc(new_layout) };
        if new_ptr.is_null() {
            return new_ptr;
        }
        // SAFETY: ptr 是旧块（layout 有效）、new_ptr 是新块（new_layout 有效）且两段
        // 不重叠；拷贝旧数据前缀，然后按**旧 layout** 归还旧块。
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
            self.dealloc(ptr, layout);
        }
        new_ptr
    }
}

/// 组件镜像的全局分配器：只在裸机 + feature `alloc` 下挂载。
/// host（`cargo test`）用 std 的分配器，adapter 类型仍被测试直接调用。
#[cfg(all(target_os = "none", feature = "alloc"))]
#[global_allocator]
static KERNEL_HEAP: KernelHeap = KernelHeap;
