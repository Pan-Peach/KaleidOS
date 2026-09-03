use bitflags::bitflags;

pub const PAGE_SIZE: usize = 4096;
pub const ENTRIES: usize = 512;
pub const LEVELS: usize = 3;

const VPN_MASK: usize = 0x1ff;
const PPN_SHIFT: usize = 10;
const PPN_MASK: usize = (1 << 44) - 1;
const LEAF_FLAGS: usize = PteFlags::R.bits() | PteFlags::W.bits() | PteFlags::X.bits();

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PteFlags: usize {
        const V = 1 << 0;
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
        const G = 1 << 5;
        const A = 1 << 6;
        const D = 1 << 7;
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pte {
    pub bits: usize,
}

impl Pte {
    pub const fn invalid() -> Self {
        Self { bits: 0 }
    }

    pub const fn new_table_pa(pa: usize) -> Self {
        Self::new_table(pa >> 12)
    }

    pub const fn new_table(ppn: usize) -> Self {
        Self {
            bits: (ppn & PPN_MASK) << PPN_SHIFT | PteFlags::V.bits(),
        }
    }

    pub const fn new_leaf_pa(pa: usize, flags: PteFlags) -> Self {
        Self::new_leaf(pa >> 12, flags)
    }

    pub const fn new_leaf(ppn: usize, flags: PteFlags) -> Self {
        Self {
            bits: (ppn & PPN_MASK) << PPN_SHIFT | flags.bits() | PteFlags::V.bits(),
        }
    }

    pub const fn is_valid(self) -> bool {
        self.bits & PteFlags::V.bits() != 0
    }

    pub const fn is_leaf(self) -> bool {
        self.bits & LEAF_FLAGS != 0
    }

    pub const fn ppn(self) -> usize {
        (self.bits >> PPN_SHIFT) & PPN_MASK
    }

    pub const fn pa(self) -> usize {
        self.ppn() << 12
    }

    pub const fn flags(self) -> PteFlags {
        PteFlags::from_bits_retain(self.bits & 0xff)
    }

    pub const fn get_pte_array(self) -> Option<&'static mut [Pte; ENTRIES]> {
        if self.is_valid() && !self.is_leaf() {
            let table_ptr = self.pa() as *mut [Pte; ENTRIES];
            Some(unsafe { &mut *table_ptr })
        } else {
            None
        }
    }
}

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
pub struct PageTable {
    pub entries: [Pte; ENTRIES],
}

impl PageTable {
    pub const fn empty() -> Self {
        Self {
            entries: [Pte::invalid(); ENTRIES],
        }
    }
}

pub const fn vpn(va: usize, level: usize) -> usize {
    (va >> (12 + level * 9)) & VPN_MASK
}

pub const fn is_page_aligned(address: usize) -> bool {
    address & (PAGE_SIZE - 1) == 0
}

pub fn find_pte(root: &mut PageTable, va: usize) -> Option<&mut Pte> {
    let mut table = &mut root.entries;

    for level in (0..=2).rev() {
        let index = vpn(va, level);
        let entry = table[index];

        if !entry.is_valid() {
            return None;
        }

        if entry.is_leaf() {
            return Some(&mut table[index]);
        }

        if level == 0 {
            return None;
        }

        table = entry.get_pte_array()?;
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    InvalidLevel,
    Unaligned,
    AlreadyMapped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pte_round_trip_preserves_pa_and_flags() {
        let pa = 0x8000_0000;
        let flags = PteFlags::R | PteFlags::W | PteFlags::A | PteFlags::D;
        let pte = Pte::new_leaf_pa(pa, flags);

        assert!(pte.is_valid());
        assert!(pte.is_leaf());
        assert_eq!(pte.pa(), pa);
        assert!(pte.flags().contains(PteFlags::R));
        assert!(pte.flags().contains(PteFlags::W));
    }

    #[test]
    fn table_pte_is_valid_but_not_leaf() {
        let pte = Pte::new_table_pa(0x8020_0000);

        assert!(pte.is_valid());
        assert!(!pte.is_leaf());
        assert_eq!(pte.pa(), 0x8020_0000);
    }

    #[test]
    fn vpn_extracts_three_nine_bit_indexes() {
        let va = 0x0000_0000_8000_1234;

        assert_eq!(vpn(va, 2), 2);
        assert_eq!(vpn(va, 1), 0);
        assert_eq!(vpn(va, 0), 1);
    }

    #[test]
    fn empty_page_table_has_no_valid_entries() {
        let table = PageTable::empty();

        assert!(table.entries.iter().all(|pte| !pte.is_valid()));
        assert!(is_page_aligned(core::mem::align_of::<PageTable>()));
    }
}
