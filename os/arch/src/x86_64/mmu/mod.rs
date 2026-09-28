//! x86_64 translation and TLB mechanism (skeleton; bodies `todo!()`).
//!
//! The address-space backend contract lives in `crate::vm`; the concrete
//! implementation for a new ISA replaces `crate::AddressSpaceImpl` (currently
//! [`crate::stub_vm::StubAddressSpace`]).  This module is where the real
//! mechanism lands.

/// Make the active page table effective on this CPU (e.g. load CR3).
pub fn activate() {
    todo!("x86_64: load the active page table (CR3)")
}

/// Invalidate the TLB after a mapping change.
pub fn flush_tlb() {
    todo!("x86_64: flush the TLB")
}
