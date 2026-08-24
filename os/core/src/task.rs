//! Task 真相：身份（TaskId）、状态、运行 CPU、上下文。
//! 调度策略数据（runqueue、vruntime 等）不在此模块 —— 属于 Scheduler Component。
//! 状态机、跨 CPU 检查等真相逻辑由人类实现；本模块只提供词汇表占位与 host test 样板。

/// 任务身份（M1 最小词汇表）—— **Identity，不是 Authority**。
/// 由 Core 分配与记录；调度器等策略组件只持有值，不持有真相。
/// 可被猜测/构造/传递（如 `TaskId(7)`），但"知道存在"≠"有权操作"；
/// 真实权限来自 Core 授予的 `TaskHandle`，任何来自 Component 的 ID 都要过 Core 验证。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[cfg(test)]
mod tests {
    use super::TaskId;

    #[test]
    fn ids_with_same_raw_are_equal() {
        assert_eq!(TaskId::from_raw(7), TaskId::from_raw(7));
    }

    #[test]
    fn ids_with_different_raw_differ() {
        assert_ne!(TaskId::from_raw(7), TaskId::from_raw(8));
    }

    #[test]
    fn raw_roundtrip() {
        assert_eq!(TaskId::from_raw(42).raw(), 42);
    }
}
