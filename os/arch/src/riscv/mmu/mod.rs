//! RISC-V address-translation backends.
//!
//! The family-level RISC-V module owns the ISA and firmware pieces; this
//! module owns the translation mechanism boundary.  The active XLEN selects
//! the Sv39 (RV64) or Sv32 (RV32) backend at compile time.

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
use super::boot_vm;

#[cfg(target_arch = "riscv64")]
const SV39_MODE: usize = 8;
#[cfg(target_arch = "riscv32")]
const SV32_MODE: usize = 1;

#[cfg(target_arch = "riscv64")]
pub type AddressSpace = address_space::Sv39AddressSpace;
#[cfg(target_arch = "riscv32")]
pub type AddressSpace = address_space::Sv32AddressSpace;

#[cfg(target_arch = "riscv64")]
pub use super::boot_vm::{KernelSection, root as get_root_table};

#[cfg(target_arch = "riscv64")]
pub unsafe fn init_identity(
    kernel_pa: usize,
    linked_kernel_pa: usize,
    image_size: usize,
    ram_base: usize,
    ram_size: usize,
    sections: &[KernelSection],
) -> Result<(), boot_vm::BootVmError> {
    unsafe {
        boot_vm::init(
            kernel_pa,
            linked_kernel_pa,
            image_size,
            ram_base,
            ram_size,
            sections,
        )
    }
}

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

/// Switch to a fresh high-half boot stack and transfer control.
///
/// The low entry still hands off through a temporary bootstrap map. The formal
/// image now uses high virtual addresses with low physical load addresses, so
/// the target itself is already high-linked while the context pointer may
/// still come from the low bootstrap stack.
#[cfg(target_arch = "riscv64")]
pub unsafe fn enter_high_half(low_entry: usize, low_context: usize, low_stack_top: usize) -> ! {
    let high_entry = boot_vm::high_alias_or_self(low_entry);
    let high_context = boot_vm::high_alias_or_self(low_context);
    let high_stack_top = boot_vm::high_alias_or_self(low_stack_top);
    unsafe {
        core::arch::asm!(
            "mv sp, {stack_top}",
            "jr {entry}",
            stack_top = in(reg) high_stack_top,
            entry = in(reg) high_entry,
            in("a0") high_context,
            options(noreturn),
        );
    }
}
