//! Task 真相：身份（TaskId）、状态、运行 CPU、上下文。
//! 调度策略数据（runqueue、vruntime 等）不在此模块 —— 属于 Scheduler Component。
//! 状态机、跨 CPU 检查等真相逻辑由人类实现；本模块只提供词汇表占位与 host test 样板。

/// 任务身份（M1 最小词汇表）。
/// 由 Core 分配与记录；调度器等策略组件只持有值，不持有真相。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskId(u32);

impl TaskId {
    /// 用原始编号构造（仅限 Core 内部使用，组件不可自行伪造）。
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