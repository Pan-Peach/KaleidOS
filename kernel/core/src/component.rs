//! ComponentId 与 ResourceDomain：组件身份、资源集合、回收顺序
//! （quiesce → stop → IRQ mask → DMA/MMIO revoke → timer cancel → resource release → destroy）。
//! ResourceDomain 的实现由人类完成；本模块只提供词汇表占位与 host test 样板。

/// 组件身份（M1 最小词汇表）。
/// 由 Core 分配；组件的 ResourceDomain 以 ComponentId 为键记录。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComponentId(u32);

impl ComponentId {
    /// 用原始编号构造（仅限 Core 内部使用）。
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
    use super::ComponentId;

    #[test]
    fn ids_with_same_raw_are_equal() {
        assert_eq!(ComponentId::from_raw(3), ComponentId::from_raw(3));
    }

    #[test]
    fn ids_with_different_raw_differ() {
        assert_ne!(ComponentId::from_raw(3), ComponentId::from_raw(4));
    }

    #[test]
    fn raw_roundtrip() {
        assert_eq!(ComponentId::from_raw(17).raw(), 17);
    }
}