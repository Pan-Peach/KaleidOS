//! aarch64 translation and TLB mechanism (skeleton; bodies `todo!()`).
//!
//! The address-space backend contract lives in `crate::vm`; the concrete
//! implementation replaces `crate::AddressSpaceImpl` for this ISA.

/// Make the active page table effective on this CPU (TTBR0/TTBR1).
pub fn activate() {
    todo!("aarch64: load the active translation tables")
}

/// Invalidate the TLB after a mapping change.
pub fn flush_tlb() {
    todo!("aarch64: TLBI")
}
