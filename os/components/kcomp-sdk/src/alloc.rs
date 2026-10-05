//! GlobalAlloc uses the deployment backend initialized by Core.
//! KernelNative shares Core's heap; private domains keep their own allocator
//! state and acquire backing only when their free list cannot satisfy a request.
//! The component uses the same allocator and artifact in either deployment.
//! Private allocation is not supported in IRQ context.

use core::alloc::{GlobalAlloc, Layout};

unsafe extern "C" {
    fn kcomp_runtime_alloc(size: usize, align: usize) -> *mut u8;
    fn kcomp_runtime_free(ptr: *mut u8, size: usize, align: usize);
}

/// Deployment adapter initialized by Core before the component's create entry.
pub struct ComponentHeap;

unsafe impl GlobalAlloc for ComponentHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: layout 由 GlobalAlloc 契约保证 size > 0、align 为 2 的幂；
        // Core 侧仍做 checked Layout 构造，非法输入返回 null。
        unsafe { kcomp_runtime_alloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr/layout 必须匹配一次成功 alloc（GlobalAlloc 契约）；Core 侧按
        // 原始 Layout 归还（slab / buddy 路由由 Layout 驱动）。
        unsafe { kcomp_runtime_free(ptr, layout.size(), layout.align()) };
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
static COMPONENT_HEAP: ComponentHeap = ComponentHeap;
