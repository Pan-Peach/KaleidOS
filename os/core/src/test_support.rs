//! Host 测试专用的**锁顺序检测**（`#[cfg(test)]`）。
//!
//! 为什么存在：host 测试共享若干**进程全局**测试锁（boundary 栈、全局堆、
//! MachineInfo、trace ring），而这些锁是 `spin::Mutex`——忙等、不主动让出。
//! 两个测试若以相反顺序持有两把锁，就会形成经典 ABBA 死锁，并在多核上退化成
//! **livelock**：自旋的线程永不 yield，把每个核都钉死在自旋里（历史上观察到
//! 测试二进制 3151% CPU、`futex_wait_queue` 永不返回）。
//!
//! 仅靠"约定"防不住：新测试随手写 `heap → boundary` 就会复发。因此所有
//! 测试锁都带**rank**，线程本地记录本线程当前持有的 rank 栈；`lock()` 在**阻塞
//! 之前**检查 `attempted.rank <= max_held_rank`，违反则立即 panic（既覆盖低 rank
//! 后取高 rank 的 ABBA，也覆盖同 rank 重入的自死锁）。panic 消息包含
//! "test lock order violation" 并点名两把锁。
//!
//! # 规范获取顺序（严格递增）
//!
//! ```text
//! SCHED(-4) → LOAD(-3) → IRQ(-2) → TIMER(-1)
//!   → BOUNDARY(0) → MACHINE(1) → MEMORY(2) → TRACE(3)
//! ```
//!
//! 负 rank 是**模块本地、最外层**的测试锁（`SCHED_TEST_LOCK` / `LOAD_TEST_LOCK`
//! / `IRQ_TEST_LOCK` / `TIMER_TEST_LOCK`）：它们在每个调用点都先于所有规范锁
//! 获取，且由检测器强制——不再是"约定"。
//!
//! 任何**新增**测试锁都必须在 `Rank` 里拿到一个新 rank 并插入这个顺序（需要时
//! 重编号，保持严格递增）；绝不允许以其他顺序持有两把测试锁。模块本地、最外层
//! 的锁用负 rank，共享的规范锁用 `0..=3`。

use core::cell::RefCell;
use spin::{Mutex, MutexGuard};

/// 测试锁的规范 rank：数值严格递增，只能按此顺序嵌套获取。
///
/// 负 rank（-4..=-1）是**模块本地、最外层**的测试锁，必须先于所有 `0..=3` 的
/// 共享规范锁获取；`0..=3` 是既有的规范顺序（值保持不变）。
#[repr(i8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Rank {
    /// 调度测试锁（`sched::tests::SCHED_TEST_LOCK`），最外层。
    Sched = -4,
    /// 组件加载测试锁（`component::load::tests::LOAD_TEST_LOCK`），最外层。
    Load = -3,
    /// IRQ 表测试锁（`irq::tests::IRQ_TEST_LOCK`），最外层。
    Irq = -2,
    /// timer 全局测试锁（`timer::tests::TIMER_TEST_LOCK`），最外层。
    Timer = -1,
    /// 组件 containment 边界栈（`containment::test_boundary_lock`）。
    Boundary = 0,
    /// 已提交的 `MachineInfo`（`machine::test_support::GUARD`）。
    Machine = 1,
    /// 全局堆（`memory::test_support::GUARD`）。
    Memory = 2,
    /// trace ring（`trace::test_support::GUARD`）。
    Trace = 3,
}

impl core::fmt::Display for Rank {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self {
            Rank::Sched => "SCHED",
            Rank::Load => "LOAD",
            Rank::Irq => "IRQ",
            Rank::Timer => "TIMER",
            Rank::Boundary => "BOUNDARY",
            Rank::Machine => "MACHINE",
            Rank::Memory => "MEMORY",
            Rank::Trace => "TRACE",
        };
        f.write_str(name)
    }
}

/// 单线程最多同时持有的测试锁数量（全序共 9 把；留余量）。
const MAX_HELD: usize = 16;

/// 本线程当前持有的 rank 栈：按获取顺序严格递增。
struct Held {
    ranks: [Rank; MAX_HELD],
    len: usize,
}

impl Held {
    const EMPTY: Self = Self {
        ranks: [Rank::Boundary; MAX_HELD],
        len: 0,
    };

    /// 违反严格递增时返回与之冲突的已持有 rank（栈顶即最大值）。
    fn conflict(&self, attempted: Rank) -> Option<Rank> {
        let max_held = if self.len == 0 {
            None
        } else {
            Some(self.ranks[self.len - 1])
        };
        max_held.filter(|held| attempted <= *held)
    }

    fn push(&mut self, rank: Rank) {
        assert!(
            self.len < MAX_HELD,
            "test lock order violation: lock nesting deeper than {MAX_HELD}"
        );
        self.ranks[self.len] = rank;
        self.len += 1;
    }

    /// 释放一个 rank（严格顺序下即栈顶；脱离顺序也不 panic——Drop 里 panic 会 abort）。
    fn release(&mut self, rank: Rank) {
        let Some(pos) = self.ranks[..self.len]
            .iter()
            .rposition(|held| *held == rank)
        else {
            return;
        };
        self.ranks.copy_within(pos + 1..self.len, pos);
        self.len -= 1;
    }
}

std::thread_local! {
    static HELD: RefCell<Held> = const { RefCell::new(Held::EMPTY) };
}

/// 带顺序检测的进程全局测试锁。用法与 `spin::Mutex` 相同：`LOCK.lock()`。
pub(crate) struct TestLock {
    rank: Rank,
    inner: Mutex<()>,
}

impl TestLock {
    /// 创建测试锁。`Rank` 决定它在规范顺序里的位置。
    pub(crate) const fn new(rank: Rank) -> Self {
        Self {
            rank,
            inner: Mutex::new(()),
        }
    }

    /// 获取锁；若本线程已持有 rank >= 自己的锁，**在阻塞前**立即 panic。
    pub(crate) fn lock(&self) -> TestLockGuard<'_> {
        if let Some(held) = HELD.with(|held| held.borrow().conflict(self.rank)) {
            panic!(
                "test lock order violation: attempted to acquire {} (rank {}) while holding {} \
                 (rank {}); canonical acquisition order is SCHED -> LOAD -> IRQ -> \
                 TIMER -> BOUNDARY -> MACHINE -> MEMORY -> TRACE",
                self.rank, self.rank as i8, held, held as i8,
            );
        }
        let guard = self.inner.lock();
        HELD.with(|held| held.borrow_mut().push(self.rank));
        TestLockGuard {
            rank: self.rank,
            _guard: guard,
        }
    }
}

/// `TestLock` 的持有凭证；`Drop` 时清掉线程本地记录（host 测试会 unwind，Drop 可靠）。
pub(crate) struct TestLockGuard<'a> {
    rank: Rank,
    _guard: MutexGuard<'a, ()>,
}

impl Drop for TestLockGuard<'_> {
    fn drop(&mut self) {
        HELD.with(|held| held.borrow_mut().release(self.rank));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模块本地锁（负 rank）一旦在规范锁之后获取 → 违反全序，阻塞前 panic。
    #[test]
    #[should_panic(expected = "test lock order violation")]
    fn module_local_rank_after_canonical_rank_panics() {
        static BOUNDARY: TestLock = TestLock::new(Rank::Boundary);
        static SCHED: TestLock = TestLock::new(Rank::Sched);

        let _boundary = BOUNDARY.lock();
        let _sched = SCHED.lock();
    }

    /// (a) 持有高 rank 锁后获取低 rank 锁（ABBA 形态）→ 立即 panic，不阻塞。
    #[test]
    #[should_panic(expected = "test lock order violation")]
    fn lower_rank_after_higher_rank_panics() {
        static HIGH: TestLock = TestLock::new(Rank::Memory);
        static LOW: TestLock = TestLock::new(Rank::Boundary);

        let _high = HIGH.lock();
        let _low = LOW.lock();
    }

    /// (b) 同 rank 重入（自死锁形态）→ 阻塞前 panic。
    #[test]
    #[should_panic(expected = "test lock order violation")]
    fn same_rank_twice_panics() {
        static LOCK: TestLock = TestLock::new(Rank::Machine);

        let _first = LOCK.lock();
        let _second = LOCK.lock();
    }
}
