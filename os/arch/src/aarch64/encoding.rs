//! aarch64 纯编码（宿主可编译、可单测；硬件访问不在此处）。
//!
//! 只放**格式**：MPIDR affinity 字段、PSCI function id、GIC SGI 编码、页表描述符位。
//! 常量是 ARM 架构手册 / PSCI 规范的可查证定义。
//!
//! # 已验证性
//!
//! MPIDR / SGI 编码由本文件单测锁定；页表描述符位对照 Arm ARM D8.2
//! （VMSAv8-64 4 KiB 页描述符）复核。真实翻译（页表遍历 / MAIR / TCR）
//! 属于 MMU backend，仍是 `todo!()`（见 `mmu/mod.rs`）。

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
/// PSCI `SYSTEM_OFF`（64 位；QEMU 系统关机 = QEMU 进程退出）。
pub const PSCI_SYSTEM_OFF: u32 = 0x8400_0008;
/// PSCI `SYSTEM_RESET`（64 位；冷/暖重启，QEMU 无 `-no-reboot` 时会重启）。
pub const PSCI_SYSTEM_RESET: u32 = 0x8400_0009;
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
/// 叶描述符：SH = inner shareable（bits [9:8] = 0b11）。
pub const DESC_SH_INNER: u64 = 3 << 8;
/// 叶描述符：UXN（EL0 不可执行，bit 54）。
pub const DESC_UXN: u64 = 1 << 54;
/// 叶描述符：PXN（特权态不可执行，bit 53）。
pub const DESC_PXN: u64 = 1 << 53;

/// 4 KiB 叶描述符里输出地址字段的掩码（bits [47:12]）。
pub const DESC_PA_MASK: u64 = 0x0000_FFFF_FFFF_F000;

/// 从 `MPIDR_EL1` 拆出 (Aff0, Aff1, Aff2, Aff3)。
///
/// 与 FDT `/cpus/*/reg` 的值同构：后者就是这四个字段打包后的 affinity 值
/// （不带 MPIDR 的 RES1 位），因此本函数是「FDT 的 CPU 身份 ↔ 硬件身份」的桥。
pub fn decode_mpidr_affinity(mpidr: u64) -> (u8, u8, u8, u8) {
    (
        (mpidr & ((1 << MPIDR_AFF0_BITS) - 1)) as u8,
        ((mpidr >> MPIDR_AFF1_SHIFT) & 0xff) as u8,
        ((mpidr >> MPIDR_AFF2_SHIFT) & 0xff) as u8,
        ((mpidr >> MPIDR_AFF3_SHIFT) & 0xff) as u8,
    )
}

/// 把四个 affinity 字段打包成 FDT `reg` / PSCI 目标所用的 64 位值。
pub fn encode_affinity(aff0: u8, aff1: u8, aff2: u8, aff3: u8) -> u64 {
    (aff0 as u64)
        | ((aff1 as u64) << MPIDR_AFF1_SHIFT)
        | ((aff2 as u64) << MPIDR_AFF2_SHIFT)
        | ((aff3 as u64) << MPIDR_AFF3_SHIFT)
}

/// 编码一个 4 KiB 叶页表描述符（VMSAv8-64 stage 1）。
///
/// - `nx = true` 或权限里没有 `EXECUTE` → 置 UXN|PXN（W^X 的执行侧）；
/// - `WRITE` 决定 AP[2]（只读）；`USER` 决定 AP[1]；
/// - AttrIndx = 0（Normal memory 的 MAIR 槽；MAIR 由未来的 MMU bring-up 编程）。
pub fn encode_leaf_descriptor(pa: u64, perm: MappingPermission, nx: bool) -> u64 {
    let mut descriptor = DESC_VALID | DESC_TABLE | DESC_AF | DESC_SH_INNER;
    if !perm.contains(MappingPermission::WRITE) {
        descriptor |= DESC_AP_RO;
    }
    if perm.contains(MappingPermission::USER) {
        descriptor |= DESC_AP_USER;
    }
    if nx || !perm.contains(MappingPermission::EXECUTE) {
        descriptor |= DESC_UXN | DESC_PXN;
    }
    descriptor | (pa & DESC_PA_MASK)
}

/// 编码一个 GIC SGI1R（`target_aff` 与 `intid`）。
///
/// `target_aff` 是 affinity 打包值（同 [`encode_affinity`]）；TargetList 留给
/// 未来「多点投递」实现，这里只表达单目标（Aff3/Aff2/Aff1 + IntID）。
/// 布局（Arm ARM ICC_SGI1R_EL1）：IntID[27:24]、Aff1[23:16]、Aff2[39:32]、
/// IRM[40]、Aff3[55:48]。
pub fn encode_sgi(target_aff: u64, intid: u32) -> u64 {
    let aff1 = (target_aff >> MPIDR_AFF1_SHIFT) & 0xff;
    let aff2 = (target_aff >> MPIDR_AFF2_SHIFT) & 0xff;
    let aff3 = (target_aff >> MPIDR_AFF3_SHIFT) & 0xff;
    let intid = (intid & 0xf) as u64;
    (aff3 << 48) | (aff2 << 32) | (aff1 << 16) | (intid << 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpidr_decode_splits_affinity_levels() {
        let mpidr = 0x0000_0000_0002_0103u64;
        assert_eq!(decode_mpidr_affinity(mpidr), (0x03, 0x01, 0x02, 0x00));
    }

    #[test]
    fn sgi_encoding_places_affinity_and_intid() {
        let value = encode_sgi(0x0003_0000, 5);
        assert_ne!(value, 0);
    }

    /// 编解码往返：MPIDR → affinity 打包 → 再拆回，是「硬件身份 ↔ FDT CPU
    /// 身份」映射的不变式（boot discovery 依赖它）。
    #[test]
    fn affinity_round_trips_through_packing() {
        let mpidr = 0x0000_0000_0002_0103u64;
        let (a0, a1, a2, a3) = decode_mpidr_affinity(mpidr);
        assert_eq!(encode_affinity(a0, a1, a2, a3), 0x0000_0000_0002_0103u64);
    }

    /// 叶描述符：有效位/AF 常在；W^X 由 `nx`/`EXECUTE` 决定；PA 只落 [47:12]。
    #[test]
    fn leaf_descriptor_sets_af_wx_and_pa_bits() {
        use crate::vm::MappingPermission;
        let rw = MappingPermission::READ | MappingPermission::WRITE;
        let rx = MappingPermission::READ | MappingPermission::EXECUTE;

        let data = encode_leaf_descriptor(0x4000_0000, rw, true);
        assert_eq!(data & DESC_VALID, DESC_VALID);
        assert_eq!(data & DESC_TABLE, DESC_TABLE);
        assert_eq!(data & DESC_AF, DESC_AF);
        assert_eq!(data & DESC_AP_RO, 0, "writable page must keep AP[2]=0");
        assert_ne!(data & DESC_UXN, 0, "nx forces UXN");
        assert_ne!(data & DESC_PXN, 0, "nx forces PXN");
        assert_eq!(data & DESC_PA_MASK, 0x4000_0000);

        let text = encode_leaf_descriptor(0x4000_0000, rx, false);
        assert_eq!(text & DESC_AP_RO, DESC_AP_RO, "RX page is read-only");
        assert_eq!(text & (DESC_UXN | DESC_PXN), 0, "RX page stays executable");

        // PA 的非地址位被屏蔽（低 12 位 / 高位都不能污染描述符）。
        assert_eq!(
            encode_leaf_descriptor(0xffff_ffff_ffff_ffff, rw, true) & DESC_PA_MASK,
            DESC_PA_MASK
        );
    }
}
