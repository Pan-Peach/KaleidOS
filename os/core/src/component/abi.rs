//! Component ABI 的共享值类型：exact ABI fingerprint + 领域分类。
//!
//! 这两个类型被**两代模型**同时使用，因此放在这里、不依赖任何一代：
//!
//! - 旧 `component/interface.rs`（全局接口名 → binding 槽）——继续 re-export，
//!   既有路径 `component::interface::InterfaceAbi` 保持不变；
//! - 新 `component/endpoint.rs`（Contract / Endpoint）——直接从这里取，
//!   不再反向依赖旧接口模块（那是迁移期必须避免的耦合）。
//!
//! `InterfaceKind` 本体是生成物（`abi/component.toml` → `generated::abi`）。

pub use crate::generated::abi::InterfaceKind;

/// Exact ABI fingerprint（`#[repr(transparent)]`，无版本兼容语义）。
///
/// 只回答："provider 与 consumer 是否由**完全相同**的 Service ABI contract
/// 编译？" 不一致 → `InterfaceError::AbiMismatch` / `EndpointError::AbiMismatch`
/// → 拒绝 binding / replacement / validate。
///
/// TODO(service-abi): 具体 Service contract 的 fingerprint 未来在 `kcomp-sdk`
/// 统一定义（例如由 contract 布局经稳定哈希生成）；当前阶段 Core 只提供 u64
/// 机制与 seam，不实现 ABI hash 生成器或 proc macro。
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InterfaceAbi(u64);

impl InterfaceAbi {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_roundtrip_and_kind_encoding_are_stable() {
        assert_eq!(
            InterfaceAbi::from_raw(0xAAAA_BBBB_CCCC_DDDD).raw(),
            0xAAAA_BBBB_CCCC_DDDD
        );
        // 领域分类 ABI 编码 0/1/2（`abi/component.toml` 单一来源）。
        assert_eq!(InterfaceKind::Device as u32, 0);
        assert_eq!(InterfaceKind::Service as u32, 1);
        assert_eq!(InterfaceKind::Policy as u32, 2);
    }
}
