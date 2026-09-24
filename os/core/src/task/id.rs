//! 任务身份：Identity，不是 Authority。
//! 由 Core 分配与记录；调度器等策略组件只持有值，不持有真相。
//! 可被猜测/构造/传递（如 `TaskId(7)`），但"知道存在"≠"有权操作"；
//! 真实权限来自 Core 授予的 `TaskHandle`，任何来自 Component 的 ID 都要过 Core 验证。

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(u32);

impl TaskId {
    /// ID 是身份标识，不是授权：可从 raw 值构造、可序列化/传递。
    /// 来自 Component/IPC/Wasm 的 ID 必须由 Core 重新验证。
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// 原始编号（供 Core 记录与 trace 使用）。
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl core::fmt::Display for TaskId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Task{}", self.0)
    }
}
