//! ComponentId 与 ResourceDomain：组件身份、资源集合、最终回收。
//! 回收不预设 universal revoke order（graceful shutdown / forced containment 双路径，
//! 见 docs/component-model.md §3）：Core 保证 eventual revocation，
//! 具体设备 shutdown 顺序由组件/驱动决定，不由 ResourceDomain 写死。
//! ResourceDomain 的实现由人类完成；本模块只提供词汇表占位与 host test 样板。

pub mod containment;
mod elf;
pub mod export;
pub mod failure;
pub mod interface;
pub mod load;
pub mod loader;
pub mod registry;
pub mod store;

pub use containment::panic_escape;
pub use failure::fail_component;

/// 组件身份（M1 最小词汇表）—— **Identity，不是 Authority**。
/// 由 Core 分配；组件的 ResourceDomain 以 ComponentId 为键记录。
/// 可被猜测/构造/传递，但真正的操作权限来自 Core 授予的组件凭证（token），
/// 任何来自 Component/IPC/Wasm 的 ID 都要过 Core 验证。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComponentId(u32);

impl ComponentId {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComponentState {
    Declared,
    /// 所有 required Interfaces 都已成功绑定（见 `component/interface.rs`）。
    /// 语义：Resolved = 依赖已就位，可以进入初始化。
    Resolved,
    Starting,
    Ready,
    Failed,
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
