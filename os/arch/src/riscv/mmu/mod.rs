//! RISC-V address-translation backends.
//!
//! The family-level RISC-V module owns the ISA and firmware pieces; this
//! module owns the translation mechanism boundary.  The active XLEN selects
//! the Sv39 (RV64) or Sv32 (RV32) backend at compile time.
//!
//! 本层只保留**机制**：页表编码/遍历（sv39/sv32）、`activate`（satp+sfence）、
//! `flush_tlb`。boot 期的映射策略（identity + high-half 双映射、段权限、
//! 临时 root、enter_high_half）已移出 arch，见 boot crate `vm/`——arch 不
//! 知道 `KERNEL_VMA` / `.text` / `.initpkg` / bootstrap hand-off。

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod address_space;
// sv32/sv39 是纯逻辑 + identity 指针解引用；host 的 cfg(test)（64 位）也编译，
// 便于 host 测试页表编码与 walk（页面 backing 由测试提供）。
#[cfg(any(target_arch = "riscv32", all(test, target_pointer_width = "64")))]
pub mod sv32;
#[cfg(any(target_arch = "riscv64", all(test, target_pointer_width = "64")))]
pub mod sv39;

#[cfg(all(test, target_pointer_width = "64"))]
pub(crate) mod test_pool;

#[cfg(target_arch = "riscv64")]
const SV39_MODE: usize = 8;
#[cfg(target_arch = "riscv32")]
const SV32_MODE: usize = 1;

#[cfg(target_arch = "riscv64")]
pub type AddressSpace = address_space::Sv39AddressSpace;
#[cfg(target_arch = "riscv32")]
pub type AddressSpace = address_space::Sv32AddressSpace;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub unsafe fn flush_tlb() {
    unsafe {
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
    }
}

/// 写 satp 并 flush TLB。这是本模块唯一职责：只碰寄存器，不懂地址空间生命周期。
/// `root_ppn` 是根页表物理页号；`asid` 是该地址空间的 ASID。
#[cfg(target_arch = "riscv64")]
pub unsafe fn activate(root_ppn: usize, asid: u16) {
    let satp = (SV39_MODE << 60) | ((asid as usize) << 44) | root_ppn;

    unsafe {
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
        core::arch::asm!(
            "csrw satp, {satp}",
            satp = in(reg) satp,
            options(nostack, preserves_flags),
        );
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
    }
}

/// Write an Sv32 `satp` value and flush stale translations.
#[cfg(target_arch = "riscv32")]
pub unsafe fn activate(root_ppn: usize, asid: u16) {
    let satp = (SV32_MODE << 31) | ((asid as usize) << 22) | root_ppn;

    unsafe {
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
        core::arch::asm!(
            "csrw satp, {satp}",
            satp = in(reg) satp,
            options(nostack, preserves_flags),
        );
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
    }
}
