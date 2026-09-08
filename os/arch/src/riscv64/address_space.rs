//! Future runtime Sv39 address-space backend boundary.
//!
//! M0.5 uses `mmu.rs` for the permanent kernel boot page table. This type is
//! intentionally not connected to boot or Core until the C10 isolation work.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sv39AddressSpace {
    root_ppn: usize,
    asid: u16,
}

impl Sv39AddressSpace {
    pub const fn placeholder() -> Self {
        Self { root_ppn: 0, asid: 0 }
    }

    pub const fn root_ppn(self) -> usize {
        self.root_ppn
    }

    pub const fn asid(self) -> u16 {
        self.asid
    }
}
