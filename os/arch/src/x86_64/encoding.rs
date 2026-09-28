//! x86_64 纯编码（宿主可编译、可单测；硬件访问不在此处）。
//!
//! 只放**格式**：寄存器的位布局、MSR 编号、页表项（PTE）位、xAPIC ICR 位。
//! 计算函数体为 `todo!()`（实现待手写）；常量是可查证的格式定义。
//!
//! # 已验证性
//!
//! 本模块是**骨架**：编码常量按 Intel SDM Vol.3 常见定义列出，实现前请复核。

use crate::vm::MappingPermission;

/// `IA32_GS_BASE` MSR（kernel per-CPU 基址）。
pub const MSR_GS_BASE: u32 = 0xC000_0101;
/// `IA32_KERNEL_GS_BASE` MSR（`swapgs` 交换的另一半）。
pub const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;
/// `IA32_APIC_BASE` MSR。
pub const MSR_APIC_BASE: u32 = 0x0000_001B;

/// `CR0.PG`（分页使能）。
pub const CR0_PG: u64 = 1 << 31;
/// `CR4.PAE`（物理地址扩展）。
pub const CR4_PAE: u64 = 1 << 5;

/// 页表项 present。
pub const PTE_PRESENT: u64 = 1 << 0;
/// 页表项 read/write。
pub const PTE_RW: u64 = 1 << 1;
/// 页表项 user/supervisor。
pub const PTE_USER: u64 = 1 << 2;
/// 页表项 accessed。
pub const PTE_ACCESSED: u64 = 1 << 5;
/// 页表项 dirty。
pub const PTE_DIRTY: u64 = 1 << 6;
/// 页表项 large page。
pub const PTE_PS: u64 = 1 << 7;
/// 页表项 no-execute。
pub const PTE_NX: u64 = 1 << 63;

/// xAPIC ICR 的 delivery-mode 字段偏移。
pub const ICR_DELIVERY_SHIFT: u64 = 8;
/// xAPIC ICR 的 destination 字段偏移。
pub const ICR_DEST_SHIFT: u64 = 56;
/// delivery mode = INIT。
pub const ICR_DELIVERY_INIT: u64 = 0b101;
/// delivery mode = startup (SIPI)。
pub const ICR_DELIVERY_SIPI: u64 = 0b110;

/// 编码一个 4 KiB 叶 PTE。
pub fn encode_leaf_pte(_pa: u64, _perm: MappingPermission, _nx: bool) -> u64 {
    todo!("x86_64: encode a 4 KiB leaf page-table entry")
}

/// 编码一个 INIT ICR（`apic_id` 为 destination）。
pub fn encode_icr_init(_apic_id: u8) -> u64 {
    todo!("x86_64: encode an INIT interrupt command")
}

/// 编码一个 SIPI ICR（`vector` 为启动向量，`apic_id` 为 destination）。
pub fn encode_icr_sipi(_vector: u8, _apic_id: u8) -> u64 {
    todo!("x86_64: encode a SIPI interrupt command")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 骨架：实现后应通过；现在 `#[ignore]` 保持基线绿，`--ignored` 可见失败。
    #[test]
    #[ignore = "new ISA skeleton: implement encode_leaf_pte"]
    fn leaf_pte_encodes_present_and_permissions() {
        let pte = encode_leaf_pte(
            0x1000,
            MappingPermission::READ | MappingPermission::WRITE,
            false,
        );
        assert_ne!(pte & PTE_PRESENT, 0);
        assert_ne!(pte & PTE_RW, 0);
    }

    #[test]
    #[ignore = "new ISA skeleton: implement encode_icr_sipi"]
    fn sipi_icr_places_vector_and_destination() {
        let icr = encode_icr_sipi(0x8, 0x3);
        assert_eq!((icr >> ICR_DELIVERY_SHIFT) & 0b111, ICR_DELIVERY_SIPI);
        assert_eq!((icr >> ICR_DEST_SHIFT) & 0xff, 0x3);
    }
}
