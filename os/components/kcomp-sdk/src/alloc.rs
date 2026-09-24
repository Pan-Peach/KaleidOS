//! alloc adapter：`GlobalAlloc` → **当前实例的堆**（[`crate::heap`]）。
//!
//! `#[global_allocator]` 不是"每个组件自带堆"，也不是 Core 共享堆：它只是把
//! Rust `GlobalAlloc` 契约路由到 [`heap::current_heap`] 指向的 per-instance
//! heap（分配器实现是 C，见 [`crate::heap`]）。
//!
//! 未设置堆（null）时**返回 null / 不动作，绝不 panic**——这是协作式
//! KernelNative 记账的诚实边界：没有堆就没有可分配的 backing。
//!
//! `#[global_allocator]` 只在裸机 + feature `alloc` 下注册；host `cargo test`
//! 编译 adapter 类型（测试直接调用它），但用 std 自己的分配器。

use crate::heap;
use core::alloc::{GlobalAlloc, Layout};

/// 薄 adapter：不拥有内存，只把 `GlobalAlloc` 路由到当前实例堆。
pub struct InstanceHeap;

unsafe impl GlobalAlloc for InstanceHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let heap = heap::current_heap();
        if heap.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: 非空 heap 必须是 Heap::place 发布的句柄（set_current_heap 的
        // 契约）；layout 由 GlobalAlloc 契约保证 size > 0、align 为 2 的幂。
        unsafe { (*heap).alloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        let heap = heap::current_heap();
        if heap.is_null() {
            return;
        }
        // SAFETY: ptr 来自同一 heap 的一次成功 alloc（GlobalAlloc 契约）。
        unsafe { (*heap).free(ptr) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let heap = heap::current_heap();
        if heap.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: ptr 来自同一 heap 的一次成功 alloc（GlobalAlloc 契约）；
        // 失败时 C 侧保证旧块原样保留。
        unsafe { (*heap).realloc(ptr, new_size, layout.align()) }
    }
}

/// 组件镜像的全局分配器：只在裸机 + feature `alloc` 下挂载。
/// host（`cargo test`）用 std 的分配器，adapter 类型仍被测试直接调用。
#[cfg(all(target_os = "none", feature = "alloc"))]
#[global_allocator]
static INSTANCE_HEAP: InstanceHeap = InstanceHeap;
