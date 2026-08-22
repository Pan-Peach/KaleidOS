//! 物理帧真相：FrameId、存在性、Free/Owned、Owner、保留区域。
//! 分配算法（buddy 树、free list）不在此模块 —— 属于 Allocator Component。
//! 所有权/状态真相逻辑由人类实现；本模块只提供词汇表占位与 host test 样板。

/// 物理帧身份（M1 最小词汇表）—— **Identity，不是 Authority**。
/// 帧的存在性、状态与所有权由 Core 记录；分配器只提议，Core 才 commit。
/// 可被猜测/构造/传递，但"知道 FrameId"≠"有权使用该帧"；
/// 真实权限来自 Core 授予的 `FrameHandle`，任何来自 Component 的 ID 都要过 Core 验证。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameId(u64);

impl FrameId {
    /// ID 是身份标识，不是授权：可从 raw 值构造、可序列化/传递。
    /// 来自 Component/IPC/Wasm 的 ID 必须由 Core 重新验证。
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