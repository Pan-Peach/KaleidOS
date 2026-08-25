//! 全局 HEAP 的 host 测试设施（`#[cfg(test)]` 专用）。
//!
//! 为什么需要：`alloc_frame()`/`free_frame()` 操作全局静态 `HEAP`，只有
//! `init()` 之后才能用——但 host 测试不会跑 bootstrap，全局堆默认未初始化。
//! 本模块提供"进程内一次性安全初始化 + 测试互斥"：
//!
//! - `GUARD`：进程级互斥，串行化所有碰全局堆的测试（并行测试下安全）
//! - `ensure_init()`：只初始化一次；backing 泄漏到进程结束
//! - backing 分配含对齐 slack（`size + FRAME_SIZE`），只把对齐后真实可用
//!   的范围交给 `try_init`——修复旧 fixture "对齐后仍给完整 size" 的越界 bug
//!
//! 任何需要 `memory::alloc_frame()` 的测试（含 task 模块）都应当：
//! `let _g = memory::test_support::GUARD.lock(); memory::test_support::ensure_init();`

use super::*;
use spin::Mutex;
use std::sync::Once;

/// 测试互斥：同一时间只允许一个使用全局堆的测试执行。
pub(crate) static GUARD: Mutex<()> = Mutex::new(());

/// GUARD 的持有类型（测试签名用）。
pub(crate) type Guard<'a> = spin::MutexGuard<'a, ()>;

static INIT: Once = Once::new();

/// 测试堆大小（4 MiB；对齐 slack 另加）。
const TEST_HEAP_SIZE: usize = 1 << 22;

/// 确保全局 HEAP 已初始化（进程内一次；重复调用无副作用）。
/// backing 泄漏到进程结束（`core::mem::forget`——测试进程生命周期即 backing 生命周期）。
pub(crate) fn ensure_init() {
    INIT.call_once(|| {
        // 分配 + 对齐 slack：向上对齐最多损失 FRAME_SIZE-1 字节，
        // 因此多分配 FRAME_SIZE 保证对齐后仍在 backing 内。
        let mut buf = std::vec![0u8; TEST_HEAP_SIZE + FRAME_SIZE];
        let base = buf.as_mut_ptr() as usize;
        let start = align_up_frame(base);
        let avail = TEST_HEAP_SIZE + FRAME_SIZE - (start - base);

        let mut heap = HEAP.lock();
        unsafe { heap.try_init(start, avail).expect("test init failed") };
        core::mem::forget(buf);
    });
}
