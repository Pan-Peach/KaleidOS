//! Sv32 page-table encoding and the dynamic translation backend.
//!
//! Sv32 is a two-level, 1024-entry translation scheme.  The backend mirrors
//! the Sv39 implementation but keeps the XLEN/format-specific constants here:
//! one 4 KiB page contains 1024 32-bit PTEs and each VPN component is 10 bits.

use crate::vm::{MappingPermission, PageAlloc, PhysicalRange, VirtualRange};
use alloc::vec::Vec;
use bitflags::bitflags;

pub const PAGE_SIZE: usize = 4096;
pub const ENTRIES: usize = 1024;
pub const LEVELS: usize = 2;

const VPN_MASK: usize = 0x3ff;
const PPN_SHIFT: usize = 10;
const PPN_MASK: u32 = (1 << 22) - 1;
const LEAF_FLAGS: u32 = PteFlags::R.bits() | PteFlags::W.bits() | PteFlags::X.bits();

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PteFlags: u32 {
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
    pub bits: u32,
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
            bits: ((ppn as u32) & PPN_MASK) << PPN_SHIFT | PteFlags::V.bits(),
        }
    }

    pub const fn new_leaf_pa(pa: usize, flags: PteFlags) -> Self {
        Self::new_leaf(pa >> 12, flags)
    }

    pub const fn new_leaf(ppn: usize, flags: PteFlags) -> Self {
        Self {
            bits: ((ppn as u32) & PPN_MASK) << PPN_SHIFT | flags.bits() | PteFlags::V.bits(),
        }
    }

    pub const fn is_valid(self) -> bool {
        self.bits & PteFlags::V.bits() != 0
    }

    pub const fn is_leaf(self) -> bool {
        self.bits & LEAF_FLAGS != 0
    }

    pub const fn ppn(self) -> usize {
        ((self.bits >> PPN_SHIFT) & PPN_MASK) as usize
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
    (va >> (12 + level * 10)) & VPN_MASK
}

pub const fn is_page_aligned(address: usize) -> bool {
    address & (PAGE_SIZE - 1) == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    InvalidLevel,
    Unaligned,
    AlreadyMapped,
    Exhausted,
    InvalidPermission,
    AddressOverflow,
}

/// Dynamic Sv32 page table.  Page-table pages come from Core's narrow
/// allocator callback and remain retained in `frames` for the lifetime of the
/// backend, matching the current Sv39 ownership model.
pub struct Sv32PageTable {
    root_ppn: usize,
    frames: Vec<usize>,
    alloc_page: PageAlloc,
}

impl Sv32PageTable {
    pub fn new(alloc_page: PageAlloc) -> Result<Self, MapError> {
        let page = alloc_page().map_err(|_| MapError::Exhausted)?;
        let root_ppn = page >> 12;
        Ok(Self {
            root_ppn,
            frames: alloc::vec![page],
            alloc_page,
        })
    }

    pub fn root_ppn(&self) -> usize {
        self.root_ppn
    }

    fn table(ppn: usize) -> &'static [Pte; ENTRIES] {
        unsafe { &*((ppn << 12) as *const [Pte; ENTRIES]) }
    }

    fn table_mut(ppn: usize) -> &'static mut [Pte; ENTRIES] {
        unsafe { &mut *((ppn << 12) as *mut [Pte; ENTRIES]) }
    }

    /// Find or create the level-0 slot for a 4 KiB mapping.
    pub fn find_pte_create(&mut self, va: usize) -> Result<&mut Pte, MapError> {
        let mut ppn = self.root_ppn;
        for level in (0..=1).rev() {
            let idx = vpn(va, level);
            let pte = &mut Self::table_mut(ppn)[idx];
            if pte.is_leaf() {
                return Ok(pte);
            }
            if level == 0 {
                return Ok(pte);
            }
            if !pte.is_valid() {
                let page = (self.alloc_page)().map_err(|_| MapError::Exhausted)?;
                self.frames.push(page);
                *pte = Pte::new_table_pa(page);
            }
            ppn = pte.ppn();
        }
        Err(MapError::Exhausted)
    }

    fn find_pte(&self, va: usize) -> Option<&Pte> {
        let mut ppn = self.root_ppn;
        for level in (0..=1).rev() {
            let pte = &Self::table(ppn)[vpn(va, level)];
            if !pte.is_valid() {
                return None;
            }
            if pte.is_leaf() {
                return Some(pte);
            }
            if level == 0 {
                return None;
            }
            ppn = pte.ppn();
        }
        None
    }

    fn find_pte_mut(&mut self, va: usize) -> Option<&mut Pte> {
        let mut ppn = self.root_ppn;
        for level in (0..=1).rev() {
            let pte = &mut Self::table_mut(ppn)[vpn(va, level)];
            if !pte.is_valid() {
                return None;
            }
            if pte.is_leaf() {
                return Some(pte);
            }
            if level == 0 {
                return None;
            }
            ppn = pte.ppn();
        }
        None
    }

    pub fn map_range(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        perm: MappingPermission,
    ) -> Result<(), MapError> {
        if va.size != pa.size
            || !is_page_aligned(va.base)
            || !is_page_aligned(pa.base)
            || !is_page_aligned(va.size)
        {
            return Err(MapError::Unaligned);
        }
        let end = va
            .base
            .checked_add(va.size)
            .ok_or(MapError::AddressOverflow)?;
        pa.base
            .checked_add(pa.size)
            .ok_or(MapError::AddressOverflow)?;
        let flags = PteFlags::try_from(perm).map_err(|_| MapError::InvalidPermission)?;
        let start = va.base;
        let mut v = start;
        let mut p = pa.base;

        let result = loop {
            if v >= end {
                break Ok(());
            }
            let pte = match self.find_pte_create(v) {
                Ok(pte) => pte,
                Err(error) => break Err(error),
            };
            if pte.is_valid() {
                break Err(MapError::AlreadyMapped);
            }
            *pte = Pte::new_leaf_pa(p, flags);
            v += PAGE_SIZE;
            p += PAGE_SIZE;
        };

        if result.is_err() {
            let _ = self.unmap_range(VirtualRange {
                base: start,
                size: v - start,
            });
        }
        result
    }

    pub fn unmap_range(&mut self, va: VirtualRange) -> Result<(), MapError> {
        let end = va
            .base
            .checked_add(va.size)
            .ok_or(MapError::AddressOverflow)?;
        let mut v = va.base;
        while v < end {
            if let Some(pte) = self.find_pte_mut(v) {
                if pte.is_valid() && pte.is_leaf() {
                    *pte = Pte::invalid();
                }
            }
            v += PAGE_SIZE;
        }
        Ok(())
    }

    pub fn translate(&self, va: usize) -> Option<usize> {
        let pte = self.find_pte(va)?;
        pte.is_valid().then(|| pte.pa() | (va & (PAGE_SIZE - 1)))
    }
}

impl TryFrom<MappingPermission> for PteFlags {
    type Error = MapError;

    fn try_from(perm: MappingPermission) -> Result<Self, Self::Error> {
        if !perm.intersects(
            MappingPermission::READ | MappingPermission::WRITE | MappingPermission::EXECUTE,
        ) {
            return Err(MapError::InvalidPermission);
        }
        if perm.contains(MappingPermission::WRITE) && !perm.contains(MappingPermission::READ) {
            return Err(MapError::InvalidPermission);
        }

        let mut flags = PteFlags::A | PteFlags::D;
        if perm.contains(MappingPermission::READ) {
            flags |= PteFlags::R;
        }
        if perm.contains(MappingPermission::WRITE) {
            flags |= PteFlags::W;
        }
        if perm.contains(MappingPermission::EXECUTE) {
            flags |= PteFlags::X;
        }
        if perm.contains(MappingPermission::USER) {
            flags |= PteFlags::U;
        }
        Ok(flags)
    }
}
