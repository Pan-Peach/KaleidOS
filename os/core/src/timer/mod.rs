//! Timer 真相：定时器句柄、到期回调归属。
//!
//! # 结构约定
//!
//! 本模块预期持续成长（sleep/超时/按组件的 `TimerHandle` 等）。实现时**按概念
//! 拆子模块、保持单文件小**（参考 `task/` 的粒度：id/state/table/error 各一个
//! 文件）——不预造空桩，等第一个真实关注点出现时落文件，避免单文件巨无霸。
//!
//! # C5 骨架（Core 机制，canonical，不做成组件）
//!
//! 单次 deadline 编程 + tick 分发 + 与调度器的抢占 seam。硬件访问走
//! `arch::TimerImpl`（`Timer` trait：`now` / `set_deadline`），Core 不感知
//! SBI/CLINT 细节；组件未来拿 `TimerHandle`（Authority ≠ Interface），
//! 本阶段只有 Core 自己消费。
//!
//! # 接线点（实现时按序）
//!
//! 1. `init`：编程第一个 deadline（`TimerImpl::set_deadline(now + period)`）；
//! 2. 时钟中断回调注册：把 [`on_trap`] 接到 arch 的 trap 分发
//!    （机制待定——arch 不依赖 Core，注册式 hook 或 boot 注入均可，
//!    见 `arch::riscv::trap::supervisor::trap_handler` 的 TODO）；
//! 3. 中断开闸：`sie.STIE` + `sstatus.SIE`（`CpuImpl` 侧原语）；
//! 4. 抢占：`on_trap` 末尾触发 `crate::sched::on_timer_tick`（模型待定）。
use arch::Timer;
use spin::Mutex;

struct TimerState {
    initialized: bool,
    ticks: u64,
    period: u64,
    next_deadline: u64,
}

static STATE: Mutex<TimerState> = Mutex::new(TimerState {
    initialized: false,
    ticks: 0,
    period: 0,
    next_deadline: 0,
});

const TICK_HZ: u64 = 100;

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

/// 初始化 Core 时钟机制：编程第一个 deadline、登记时钟回调、开中断。
///
/// TODO(C5)：实现（接线点见模块文档；频率 = timebase Hz，period = 1/frequency）。
pub fn init(frequency_hz: usize) -> Result<(), TimerError> {
    let timebase_hz = u64::try_from(frequency_hz).map_err(|_| TimerError::InvalidFrequency)?;
    if timebase_hz == 0 || timebase_hz < TICK_HZ {
        return Err(TimerError::InvalidFrequency);
    }
    let period = timebase_hz / TICK_HZ;
    if period == 0 {
        return Err(TimerError::InvalidFrequency);
    }
    let first_deadline = arch::TimerImpl::now()
        .checked_add(period)
        .ok_or(TimerError::InvalidFrequency)?;

    {
        let mut state = STATE.lock();
        if state.initialized {
            return Err(TimerError::AlreadyInitialized);
        }
        state.initialized = true;
        state.period = period;
        state.next_deadline = first_deadline;
    }

    arch::TimerImpl::set_deadline(first_deadline);
    arch::TimerImpl::register_timer_handler(on_trap);
    arch::TimerImpl::enable_timer_interrupt();
    Ok(())
}

/// 时钟中断入口（trap 分发调用；中断上下文，已关中断）。
///
/// 职责：重编程下一次 deadline + tick 计数 + 触发调度抢占 seam
/// （`crate::sched::on_timer_tick`）。
///
/// TODO(C5)：实现；抢占模型（延迟重调度 vs trap 内直接切换）见 sched 侧注记。
pub extern "C" fn on_trap() {
    let now = arch::TimerImpl::now();
    let next = {
        let mut state = STATE.lock();
        if !state.initialized {
            return;
        }
        state.ticks += 1;
        while state.next_deadline <= now {
            state.next_deadline = state.next_deadline.saturating_add(state.period);
        }
        state.next_deadline
    };

    arch::TimerImpl::set_deadline(next);
}

/// 已过去的 tick 数（观测/测试用）。
///
/// TODO(C5)：实现。
pub fn ticks() -> u64 {
    let _irq_guard = crate::irq::IrqSaveGuard::new();
    STATE.lock().ticks
}
