use super::boot_vm;

const SV39_MODE: usize = 8;

pub use super::boot_vm::{KernelSection, root as get_root_table};

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

/// Switch to a fresh high-half boot stack and transfer control.
///
/// The low entry still hands off through a temporary bootstrap map. The formal
/// image now uses high virtual addresses with low physical load addresses, so
/// the target itself is already high-linked while the context pointer may
/// still come from the low bootstrap stack.
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
