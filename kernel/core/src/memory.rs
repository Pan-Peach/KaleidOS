//! 物理帧真相：FrameId、存在性、Free/Owned、Owner、保留区域。
//! 分配算法（buddy 树、free list）不在此模块 —— 属于 Allocator Component。
//! 所有权/状态真相逻辑由人类实现；本模块只提供词汇表占位与 host test 样板。

/// 物理帧身份（M1 最小词汇表）。
/// 帧的存在性、状态与所有权由 Core 记录；分配器只提议，Core 才 commit。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameId(u64);

impl FrameId {
    /// 用原始编号构造（仅限 Core 内部使用）。
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// 原始编号（供 Core 记录与 trace 使用）。
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::FrameId;

    #[test]
    fn ids_with_same_raw_are_equal() {
        assert_eq!(FrameId::from_raw(100), FrameId::from_raw(100));
    }

    #[test]
    fn ids_with_different_raw_differ() {
        assert_ne!(FrameId::from_raw(100), FrameId::from_raw(101));
    }

    #[test]
    fn raw_roundtrip() {
        assert_eq!(FrameId::from_raw(u64::MAX).raw(), u64::MAX);
    }
}