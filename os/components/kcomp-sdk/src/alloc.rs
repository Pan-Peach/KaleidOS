//! alloc adapter（feature `alloc`）：`GlobalAlloc` → Core 共享堆。
//!
//! `#[global_allocator]` 不是"每个组件自带堆"：它只是把 Rust `GlobalAlloc`
//! 契约接到 **Core 共享堆**（`kcore_heap_alloc/dealloc`）。默认关闭，组件按需
//! 通过 `kcomp-sdk = { path = "...", features = ["alloc"] }` 开启。

use crate::abi;
use core::alloc::{GlobalAlloc, Layout};

/// 薄 adapter：不拥有内存，只把 Rust `GlobalAlloc` 契约转成 Core 共享堆 ABI。
struct CoreHeap;

unsafe impl GlobalAlloc for CoreHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: layout 由 GlobalAlloc 契约保证合法（size>0、align 为 2 的幂）；
        // Core 侧再次校验，失败返回 null。
        unsafe { abi::kcore_heap_alloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr 来自同一 layout 的一次成功 alloc（GlobalAlloc 契约）。
        unsafe {
            let _ = abi::kcore_heap_dealloc(ptr, layout.size(), layout.align());
        }
    }
}

#[global_allocator]
static CORE_HEAP: CoreHeap = CoreHeap;
