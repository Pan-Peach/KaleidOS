//! 任务状态：Core 校验后的状态机。

use crate::machine::CpuId;

/// 任务状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    /// 任务已创建，但尚未调度运行。
    Created,
    /// 任务已被调度，但尚未开始运行（等待 CPU）。
    Runnable,
    /// 任务正在运行（在某个 CPU 上）。
    Running(CpuId),
    /// 任务暂不参与调度，等待 owner 再次 unpark。
    Blocked,
    /// 任务已完成（退出）。
    Exited,
}
