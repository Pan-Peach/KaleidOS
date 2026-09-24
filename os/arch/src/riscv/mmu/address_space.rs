//! RISC-V projection of a Core `KernelAddressSpace`.
//!
//! The Core-facing contract is shared, while the concrete page-table type is
//! selected by XLEN: RV64 uses Sv39 and RV32 uses Sv32.

use crate::vm::{AddressSpaceBackend, MappingPermission, PageAlloc, PhysicalRange, VirtualRange};

#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
use super::sv32::{self, MapError, Sv32PageTable};
#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
use super::sv39::{self, MapError, Sv39PageTable};

#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
pub struct Sv39AddressSpace {
    asid: u16,
    table: Sv39PageTable,
}

#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
impl Sv39AddressSpace {
    pub fn new(alloc: PageAlloc, asid: u16) -> Result<Self, MapError> {
        Ok(Self {
            asid,
            table: Sv39PageTable::new(alloc)?,
        })
    }

    pub fn root_ppn(&self) -> usize {
        self.table.root_ppn()
    }

    pub fn asid(&self) -> u16 {
        self.asid
    }
}

#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
impl AddressSpaceBackend for Sv39AddressSpace {
    type Error = sv39::MapError;
    const GRANULE: usize = sv39::VM_PAGE_SIZE;

    fn create(alloc: PageAlloc) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Self::new(alloc, 0)
    }

    fn map(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        perm: MappingPermission,
    ) -> Result<(), Self::Error> {
        self.table.map_range(va, pa, perm)
    }

    fn unmap(&mut self, va: VirtualRange) -> Result<(), Self::Error> {
        self.table.unmap_range(va)
    }

    fn translate(&self, va: usize) -> Option<usize> {
        self.table.translate(va)
    }

    fn activate(&self) -> Result<(), Self::Error> {
        unsafe {
            super::activate(self.table.root_ppn(), self.asid);
        }
        Ok(())
    }
}

#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
pub struct Sv32AddressSpace {
    asid: u16,
    table: Sv32PageTable,
}

#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
impl Sv32AddressSpace {
    pub fn new(alloc: PageAlloc, asid: u16) -> Result<Self, MapError> {
        Ok(Self {
            asid,
            table: Sv32PageTable::new(alloc)?,
        })
    }

    pub fn root_ppn(&self) -> usize {
        self.table.root_ppn()
    }

    pub fn asid(&self) -> u16 {
        self.asid
    }
}

#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
impl AddressSpaceBackend for Sv32AddressSpace {
    type Error = sv32::MapError;
    const GRANULE: usize = sv32::VM_PAGE_SIZE;

    fn create(alloc: PageAlloc) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Self::new(alloc, 0)
    }

    fn map(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        perm: MappingPermission,
    ) -> Result<(), Self::Error> {
        self.table.map_range(va, pa, perm)
    }

    fn unmap(&mut self, va: VirtualRange) -> Result<(), Self::Error> {
        self.table.unmap_range(va)
    }

    fn translate(&self, va: usize) -> Option<usize> {
        self.table.translate(va)
    }

    fn activate(&self) -> Result<(), Self::Error> {
        unsafe {
            super::activate(self.table.root_ppn(), self.asid);
        }
        Ok(())
    }
}
