//! Component ABI 的共享值类型：exact ABI fingerprint + 领域分类。
//!
//! Contract / Endpoint 模型（`component/endpoint.rs`）与所有导出面直接从这里取，
//! 不依赖任何具体 registry 模块。`InterfaceKind` 本体是生成物
//! （`abi/component.toml` → `generated::abi`）。

pub use crate::generated::abi::InterfaceKind;

/// Exact ABI fingerprint（`#[repr(transparent)]`，无版本兼容语义）。
///
/// 只回答："provider 与 consumer 是否由**完全相同**的 Service ABI contract
/// 编译？" 不一致 → `EndpointError::AbiMismatch` → 拒绝 publish / validate / bind。
///
/// 具体 Service contract 的 fingerprint 在 `kcomp-sdk` 统一定义（例如由
/// contract 布局经稳定哈希生成）；Core 只提供 u64 机制与 seam，不实现 ABI
/// hash 生成器或 proc macro。
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
