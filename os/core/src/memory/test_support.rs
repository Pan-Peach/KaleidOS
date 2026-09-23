//! 全局 HEAP 的 host 测试设施（`#[cfg(test)]` 专用）。
//!
//! 为什么需要：`alloc_region()`/`free_region()` 操作全局静态 `HEAP`，只有
//! `init()` 之后才能用——但 host 测试不会跑 bootstrap，全局堆默认未初始化。
//! 本模块提供"进程内一次性安全初始化 + 测试互斥"：
//!
//! - `GUARD`：进程级互斥，串行化所有碰全局堆的测试（并行测试下安全）
//! - `ensure_init()`：只初始化一次；backing 泄漏到进程结束
//! - backing 分配含对齐 slack（`size + ALLOC_GRANULE`），只把对齐后真实可用
//!   的范围交给 `try_init`——修复旧 fixture "对齐后仍给完整 size" 的越界 bug
//!
//! 任何需要 `memory::alloc_region()` 的测试（含 task 模块）都应当：
//! `let _g = memory::test_support::GUARD.lock(); memory::test_support::ensure_init();`

use super::*;
use crate::test_support::{Rank, TestLock, TestLockGuard};
use std::sync::Once;

/// 测试互斥：同一时间只允许一个使用全局堆的测试执行。
///
/// rank = MEMORY（规范顺序 `SCHED → LOAD → INSPECTOR → IRQ → TIMER → BOUNDARY → MACHINE → MEMORY → TRACE`；见
/// [`crate::test_support`]）。
pub(crate) static GUARD: TestLock = TestLock::new(Rank::Memory);

/// GUARD 的持有类型（测试签名用）。
pub(crate) type Guard<'a> = TestLockGuard<'a>;

static INIT: Once = Once::new();

/// 测试堆大小（4 MiB；对齐 slack 另加）。
const TEST_HEAP_SIZE: usize = 1 << 22;

/// 静态 backing：位于测试二进制的 .bss（与代码同段），模拟"组件与内核同物理区"——
/// 重定位的 ±2GB PC-relative 约束在 host 上也成立（堆分配的地址会被 ASLR 打散）。
#[cfg(test)]
static mut TEST_HEAP_BACKING: [u8; TEST_HEAP_SIZE + super::ALLOC_GRANULE] =
    [0; TEST_HEAP_SIZE + super::ALLOC_GRANULE];

/// 确保全局 HEAP 已初始化（进程内一次；重复调用无副作用）。
pub(crate) fn ensure_init() {
    INIT.call_once(|| {
        let ptr = core::ptr::addr_of_mut!(TEST_HEAP_BACKING) as usize;
        let start = super::align_up_page(ptr);
        let avail = TEST_HEAP_SIZE + super::ALLOC_GRANULE - (start - ptr);

        let mut heap = HEAP.lock();
        unsafe { heap.try_init(start, avail).expect("test init failed") };
    });
}
