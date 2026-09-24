//! Timer 真相：定时器句柄、到期回调归属。
//!
//! # 结构约定
//!
//! 本模块预期持续成长（sleep/超时/按组件的 `TimerHandle` 等）。实现时**按概念
//! 拆子模块、保持单文件小**（参考 `task/` 的粒度：id/state/table/error 各一个
//! 文件）——不预造空桩，等第一个真实关注点出现时落文件，避免单文件巨无霸。
//!
//! # 定位（Core 机制，canonical，不做成组件）
//!
//! 单次 deadline 编程 + tick 分发 + 与调度器的抢占 seam。硬件访问走
//! `arch::TimerImpl`（`Timer` trait：`now` / `set_deadline`），Core 不感知
//! SBI/CLINT 细节；当前只有 Core 自己消费（组件 `TimerHandle` 未实现）。
//!
//! # 接线点
//!
//! 1. `init`：登记 trap 回调并打开 timer interrupt，不自动产生周期 tick；
//! 2. `arm_deadline`：为下一个 sleep/timeout/event 编程 one-shot deadline；
//! 3. 时钟中断回调注册：把 [`on_trap`] 接到 arch 的 trap 分发
//!    （机制待定——arch 不依赖 Core，注册式 hook 或 boot 注入均可，
//!    见 `arch::riscv::trap::supervisor::trap_handler` 的 TODO）；
//! 4. 中断开闸：`sie.STIE` + `sstatus.SIE`（`CpuImpl` 侧原语）；
//! 5. 可选抢占：仅 `preempt` profile 由 `init_preempt` 使用周期 deadline。
use arch::Timer;
use spin::Mutex;

struct TimerState {
    initialized: bool,
    ticks: u64,
    next_deadline: Option<u64>,
    #[cfg(feature = "preempt")]
    preempt_period: Option<u64>,
}

static STATE: Mutex<TimerState> = Mutex::new(TimerState {
    initialized: false,
    ticks: 0,
    next_deadline: None,
    #[cfg(feature = "preempt")]
    preempt_period: None,
});

#[cfg(feature = "preempt")]
const PREEMPT_HZ: u64 = 100;

/// 时钟机制错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerError {
    /// 频率非法（0 或换算溢出）。
    InvalidFrequency,
    /// 已经初始化（Core 单例机制，只 init 一次）。
    AlreadyInitialized,
    /// 未初始化（`on_trap` / `ticks` 先于 `init`）。
    NotInitialized,
}

/// 初始化 one-shot timer 机制：登记时钟回调并打开 timer interrupt。
///
/// 此函数不会自动编程 deadline；没有事件时，cooperative profile 不会产生
/// 周期性 timer IRQ。事件机制通过 [`arm_deadline`] 编程下一次到期时间。
pub fn init() -> Result<(), TimerError> {
    {
        let mut state = STATE.lock();
        if state.initialized {
            return Err(TimerError::AlreadyInitialized);
        }
        state.initialized = true;
    }

    arch::TimerImpl::register_timer_handler(on_trap);
    arch::TimerImpl::enable_timer_interrupt();
    Ok(())
}

/// 编程下一次 one-shot deadline。
pub fn arm_deadline(deadline: u64) -> Result<(), TimerError> {
    let _irq_guard = crate::irq::IrqSaveGuard::new();
    {
        let mut state = STATE.lock();
        if !state.initialized {
            return Err(TimerError::NotInitialized);
        }
        state.next_deadline = Some(deadline);
    }
    arch::TimerImpl::set_deadline(deadline);
    Ok(())
}

#[cfg(feature = "preempt")]
/// 为抢占 profile 初始化周期性调度 tick。
pub fn init_preempt(timebase_hz: usize) -> Result<(), TimerError> {
    let timebase_hz = u64::try_from(timebase_hz).map_err(|_| TimerError::InvalidFrequency)?;
    if timebase_hz < PREEMPT_HZ {
        return Err(TimerError::InvalidFrequency);
    }
    let period = timebase_hz / PREEMPT_HZ;
    if period == 0 {
        return Err(TimerError::InvalidFrequency);
    }
    let first_deadline = arch::TimerImpl::now()
        .checked_add(period)
        .ok_or(TimerError::InvalidFrequency)?;
    init()?;
    {
        let mut state = STATE.lock();
        state.preempt_period = Some(period);
    }
    arm_deadline(first_deadline)
}

/// 时钟中断入口（trap 分发调用；中断上下文，已关中断）。
///
/// 职责：重编程下一次 deadline + tick 计数 + 触发调度抢占 seam
/// （`crate::sched::on_timer_tick`）。
///
/// 抢占模型（延迟重调度 vs trap 内直接切换）见 [`crate::sched::on_timer_tick`]。
pub extern "C" fn on_trap() {
    #[cfg(feature = "preempt")]
    let now = arch::TimerImpl::now();
    let next = {
        let mut state = STATE.lock();
        if !state.initialized {
            return;
        }
        state.ticks += 1;
        #[cfg(feature = "preempt")]
        if let Some(period) = state.preempt_period {
            let mut next = state.next_deadline.unwrap_or(now);
            while next <= now {
                next = next.saturating_add(period);
            }
            state.next_deadline = Some(next);
        }
        #[cfg(not(feature = "preempt"))]
        {
            state.next_deadline = None;
        }
        state.next_deadline
    };

    if let Some(next) = next {
        arch::TimerImpl::set_deadline(next);
    } else {
        arch::TimerImpl::cancel_deadline();
    }
}

/// 已过去的 tick 数（观测/测试用）。
pub fn ticks() -> u64 {
    let _irq_guard = crate::irq::IrqSaveGuard::new();
    STATE.lock().ticks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Rank, TestLock};

    /// 序列化触碰进程级 timer 全局的测试。
    ///
    /// `STATE` 是进程级 `static`，`init()` 每个进程只能成功一次且无法重置，
    /// 所以整条生命周期必须放在单个 `#[test]` 里。锁本身沿用 irq / sched /
    /// containment / trace 的纪律，防止新增测试并发改动同一全局。
    ///
    /// rank = TIMER（模块本地、最外层；见 [`crate::test_support`]）。
    static TIMER_TEST_LOCK: TestLock = TestLock::new(Rank::Timer);

    /// 验收：one-shot timer 机制在 host 上的完整生命周期（未初始化 → init →
    /// 编程 deadline → trap 计数 → 一次性清除 deadline）。
    #[test]
    fn timer_lifecycle_covers_init_arm_trap_and_ticks() {
        let _serial = TIMER_TEST_LOCK.lock();

        // Given: 机制尚未初始化。
        assert!(!STATE.lock().initialized);

        // When: 未初始化时编程 deadline。
        let armed = arm_deadline(123);

        // Then: 被拒绝（未初始化），且没有 tick 产生。
        assert_eq!(armed, Err(TimerError::NotInitialized));
        assert_eq!(ticks(), 0);

        // When: 未初始化时时钟 trap 到达。
        on_trap();

        // Then: not-initialized 早退路径不改变 tick 计数。
        assert_eq!(ticks(), 0);

        // When: 首次 init。
        let first = init();

        // Then: 首次成功；单例机制拒绝第二次 init。
        assert_eq!(first, Ok(()));
        assert_eq!(init(), Err(TimerError::AlreadyInitialized));

        // When: init 之后编程 deadline。
        let armed = arm_deadline(500);

        // Then: 这次被接受并记录（One-shot 语义：只等这一次）。
        assert_eq!(armed, Ok(()));
        assert_eq!(STATE.lock().next_deadline, Some(500));

        // When: 时钟 trap 连续到达 3 次。
        on_trap();
        on_trap();
        on_trap();

        // Then: 每次 trap 都推进 tick 计数。
        assert_eq!(ticks(), 3);

        // When/Then: 默认（非 preempt）profile 下 on_trap 清除待处理的
        // deadline（one-shot 语义），后续 trap 继续计数。
        #[cfg(not(feature = "preempt"))]
        assert_eq!(STATE.lock().next_deadline, None);
        on_trap();
        assert_eq!(ticks(), 4);
    }
}
