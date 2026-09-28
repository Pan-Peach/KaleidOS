//! aarch64 纯编码（宿主可编译、可单测；硬件访问不在此处）。
//!
//! 只放**格式**：MPIDR affinity 字段、PSCI function id、GIC SGI 编码、页表描述符位。
//! 计算函数体为 `todo!()`；常量是 ARM 架构手册 / PSCI 规范的可查证定义。
//!
//! # 已验证性
//!
//! 骨架：实现前请对照 Arm ARM 与 PSCI 规范复核。

use crate::vm::MappingPermission;

/// `MPIDR_EL1` affinity level 0 的位宽。
pub const MPIDR_AFF0_BITS: u32 = 8;
/// `MPIDR_EL1` affinity level 1 的位偏移。
pub const MPIDR_AFF1_SHIFT: u32 = 8;
/// `MPIDR_EL1` affinity level 2 的位偏移。
pub const MPIDR_AFF2_SHIFT: u32 = 16;
/// `MPIDR_EL1` affinity level 3 的位偏移。
pub const MPIDR_AFF3_SHIFT: u32 = 32;

/// PSCI `CPU_ON`（64 位 SMC）。
pub const PSCI_CPU_ON: u32 = 0xC400_0003;
/// PSCI `CPU_OFF`（64 位 SMC）。
pub const PSCI_CPU_OFF: u32 = 0x8400_0002;
/// PSCI 成功返回码。
pub const PSCI_SUCCESS: i64 = 0;

/// SGI 中断号合法范围下界。
pub const SGI_MIN: u32 = 0;
/// SGI 中断号合法范围上界（含）。
pub const SGI_MAX: u32 = 15;

/// 页表描述符 valid 位。
pub const DESC_VALID: u64 = 1 << 0;
/// 页表描述符 table/page 位。
pub const DESC_TABLE: u64 = 1 << 1;
/// 叶描述符：特权态可读/可写（AP[2:1] = 00）。注意 00 即“可写”，不是位或语义。
pub const DESC_AP_RW: u64 = 0;
/// 叶描述符：user 可访问（AP[1] = 1）。
pub const DESC_AP_USER: u64 = 1 << 6;
/// 叶描述符：只读（AP[2] = 1）。
pub const DESC_AP_RO: u64 = 1 << 7;
/// 叶描述符：AF（access flag）。
pub const DESC_AF: u64 = 1 << 10;

/// 从 `MPIDR_EL1` 拆出 (Aff0, Aff1, Aff2, Aff3)。
pub fn decode_mpidr_affinity(_mpidr: u64) -> (u8, u8, u8, u8) {
    todo!("aarch64: decode MPIDR_EL1 affinity fields")
}

/// 编码一个 4 KiB 叶页表描述符。
pub fn encode_leaf_descriptor(_pa: u64, _perm: MappingPermission, _nx: bool) -> u64 {
    todo!("aarch64: encode a 4 KiB leaf descriptor")
}

/// 编码一个 GIC SGI1R（`target_aff` 与 `intid`）。
pub fn encode_sgi(_target_aff: u64, _intid: u32) -> u64 {
    todo!("aarch64: encode an ICC_SGI1R_EL1 value")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 骨架：实现后应通过；现在 `#[ignore]` 保持基线绿，`--ignored` 可见失败。
    #[test]
    #[ignore = "new ISA skeleton: implement decode_mpidr_affinity"]
    fn mpidr_decode_splits_affinity_levels() {
        let mpidr = 0x0000_0000_0002_0103u64;
        assert_eq!(decode_mpidr_affinity(mpidr), (0x03, 0x01, 0x02, 0x00));
    }

    #[test]
    #[ignore = "new ISA skeleton: implement encode_sgi"]
    fn sgi_encoding_places_affinity_and_intid() {
        let value = encode_sgi(0x0003_0000, 5);
        assert_ne!(value, 0);
    }
}
