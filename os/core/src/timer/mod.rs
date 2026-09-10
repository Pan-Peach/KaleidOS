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
pub fn init(_frequency_hz: usize) -> Result<(), TimerError> {
    todo!("C5: timer::init")
}

/// 时钟中断入口（trap 分发调用；中断上下文，已关中断）。
///
/// 职责：重编程下一次 deadline + tick 计数 + 触发调度抢占 seam
/// （`crate::sched::on_timer_tick`）。
///
/// TODO(C5)：实现；抢占模型（延迟重调度 vs trap 内直接切换）见 sched 侧注记。
pub fn on_trap() {
    todo!("C5: timer::on_trap")
}

/// 已过去的 tick 数（观测/测试用）。
///
/// TODO(C5)：实现。
pub fn ticks() -> u64 {
    todo!("C5: timer::ticks")
}
