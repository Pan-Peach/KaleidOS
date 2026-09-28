//! loongarch64 translation and TLB mechanism (skeleton; bodies `todo!()`).
//!
//! The address-space backend contract lives in `crate::vm`; the concrete
//! implementation replaces `crate::AddressSpaceImpl` for this ISA.

/// Make the active page table effective on this CPU (CSR.PGD/PWCL/PWCH).
pub fn activate() {
    todo!("loongarch64: load the active page directory")
}

/// Invalidate the TLB after a mapping change.
pub fn flush_tlb() {
    todo!("loongarch64: invalidate TLB entries")
}
