//! aarch64 register context layout and switching (skeleton; bodies `todo!()`).
//!
//! The layout lives beside the switch assembly and must stay byte-identical.

/// Switch from `from` to `to`.  Implemented in assembly.
///
/// # Safety
/// Both pointers must reference live, correctly-aligned context records; runs
/// with interrupts disabled and must not be entered concurrently on one CPU.
pub unsafe fn switch(
    _from: *mut super::cpu::Aarch64Context,
    _to: *const super::cpu::Aarch64Context,
) {
    todo!("aarch64: context switch (assembly)")
}
