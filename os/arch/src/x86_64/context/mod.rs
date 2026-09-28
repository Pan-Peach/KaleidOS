//! x86_64 register context layout and switching (skeleton; bodies `todo!()`).
//!
//! Mirrors `riscv/context/`: the layout lives beside the switch assembly and
//! must stay byte-identical to it.

/// Switch from `from` to `to`.  Implemented in assembly.
///
/// # Safety
/// Both pointers must reference live, correctly-aligned context records; the
/// switch runs with interrupts disabled and must not be entered concurrently
/// on the same CPU.
pub unsafe fn switch(_from: *mut super::cpu::X86_64Context, _to: *const super::cpu::X86_64Context) {
    todo!("x86_64: context switch (assembly)")
}
