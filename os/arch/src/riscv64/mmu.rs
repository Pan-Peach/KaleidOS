use super::boot_vm;

const SV39_MODE: usize = 8;

pub use super::boot_vm::root as get_root_table;

pub unsafe fn init_identity(
    kernel_pa: usize,
    ram_base: usize,
    ram_size: usize,
) -> Result<(), boot_vm::BootVmError> {
    unsafe { boot_vm::init(kernel_pa, ram_base, ram_size) }
}

pub unsafe fn activate() {
    let root_pa = boot_vm::root_pa();
    let satp = (SV39_MODE << 60) | (root_pa >> 12);

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

/// Temporary H0 probe: jump from a low identity VA to its high-half alias.
/// The target must be a position-independent assembly probe for now; a full
/// high-linked Rust entry comes with the linker VMA/LMA transition later.
pub unsafe fn jump_to_high_alias(low_entry: usize) -> ! {
    let high_entry = boot_vm::high_alias_of(low_entry);
    unsafe {
        core::arch::asm!(
            "jr {entry}",
            entry = in(reg) high_entry,
            options(noreturn),
        );
    }
}
