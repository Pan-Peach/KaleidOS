//! x86_64 纯编码（宿主可编译、可单测；硬件访问不在此处）。
//!
//! 只放**格式**：寄存器的位布局、MSR 编号、页表项（PTE）位、xAPIC ICR 位。
//!
//! # 已验证性
//!
//! 位布局按 Intel SDM Vol.3 定义；`encode_leaf_pte` 与 ICR 编码由本模块的
//! 单元测试锁定（host `cargo test -p arch`）。

use crate::vm::MappingPermission;

/// `IA32_FS_BASE` MSR（FS base；与 per-CPU GS base 分离的架构状态载体）。
pub const MSR_FS_BASE: u32 = 0xC000_0100;
/// `IA32_GS_BASE` MSR（kernel per-CPU 基址）。
pub const MSR_GS_BASE: u32 = 0xC000_0101;
/// `IA32_KERNEL_GS_BASE` MSR（`swapgs` 交换的另一半）。
pub const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;
/// `IA32_EFER` MSR（long mode 使能位 `LME`）。
pub const MSR_EFER: u32 = 0xC000_0080;
/// `IA32_EFER.LME`：long mode enable。
pub const EFER_LME: u64 = 1 << 8;
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

/// 编码一个 4 KiB 叶 PTE（`PS=0`）。
///
/// - `PTE_PRESENT` 恒置位；
/// - `MappingPermission::WRITE` → `PTE_RW`（缺省只读）；
/// - `MappingPermission::USER` → `PTE_USER`（缺省 supervisor）；
/// - `nx` → `PTE_NX`。**x86 没有正向的 execute 位**：可执行 = `PTE_NX == 0`；
///   `MappingPermission::EXECUTE` 由调用方翻译成 `nx = false`，本函数不反向推导。
pub fn encode_leaf_pte(pa: u64, perm: MappingPermission, nx: bool) -> u64 {
    let mut pte = (pa & !0xfff) | PTE_PRESENT;
    if perm.contains(MappingPermission::WRITE) {
        pte |= PTE_RW;
    }
    if perm.contains(MappingPermission::USER) {
        pte |= PTE_USER;
    }
    if nx {
        pte |= PTE_NX;
    }
    pte
}

/// 编码一个 INIT ICR（`apic_id` 为 destination）。
pub fn encode_icr_init(apic_id: u8) -> u64 {
    (ICR_DELIVERY_INIT << ICR_DELIVERY_SHIFT) | ((apic_id as u64) << ICR_DEST_SHIFT)
}

/// 编码一个 SIPI ICR（`vector` 为启动向量，`apic_id` 为 destination）。
pub fn encode_icr_sipi(vector: u8, apic_id: u8) -> u64 {
    (ICR_DELIVERY_SIPI << ICR_DELIVERY_SHIFT)
        | (vector as u64)
        | ((apic_id as u64) << ICR_DEST_SHIFT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_pte_encodes_present_and_permissions() {
        let pte = encode_leaf_pte(
            0x1000,
            MappingPermission::READ | MappingPermission::WRITE,
            false,
        );
        assert_ne!(pte & PTE_PRESENT, 0);
        assert_ne!(pte & PTE_RW, 0);
        assert_eq!(pte & PTE_NX, 0, "no NX means executable");
        assert_eq!(
            pte & !0xfff,
            0x1000,
            "physical address is page-aligned in the PTE"
        );
    }

    /// 只读映射不得置 `PTE_RW`；`nx` 置 `PTE_NX`；`USER` 置 `PTE_USER`。
    #[test]
    fn leaf_pte_read_only_nx_and_user_bits() {
        let pte = encode_leaf_pte(
            0x2_0000,
            MappingPermission::READ | MappingPermission::USER,
            true,
        );
        assert_ne!(pte & PTE_PRESENT, 0);
        assert_eq!(pte & PTE_RW, 0, "read-only leaf");
        assert_ne!(pte & PTE_USER, 0, "user leaf");
        assert_ne!(pte & PTE_NX, 0, "nx leaf");
        assert_eq!(pte & PTE_PS, 0, "4 KiB leaf must not set PS");
    }

    #[test]
    fn sipi_icr_places_vector_and_destination() {
        let icr = encode_icr_sipi(0x8, 0x3);
        assert_eq!((icr >> ICR_DELIVERY_SHIFT) & 0b111, ICR_DELIVERY_SIPI);
        assert_eq!((icr >> ICR_DEST_SHIFT) & 0xff, 0x3);
        assert_eq!(icr & 0xff, 0x8, "startup vector is the low byte");
    }

    #[test]
    fn init_icr_places_delivery_mode_and_destination() {
        let icr = encode_icr_init(0x5);
        assert_eq!((icr >> ICR_DELIVERY_SHIFT) & 0b111, ICR_DELIVERY_INIT);
        assert_eq!((icr >> ICR_DEST_SHIFT) & 0xff, 0x5);
        assert_eq!(icr & 0xff, 0, "INIT has no vector (delivery mode only)");
    }
}
