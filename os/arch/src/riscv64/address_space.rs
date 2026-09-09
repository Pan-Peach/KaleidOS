//! RISC-V projection of a `KernelAddressSpace`.
//!
//! Holds the root PPN, the ASID and the concrete `Sv39PageTable` backend.
//! It translates Core-approved mappings into PTE writes via that backend.

use super::{
    mmu,
    sv39::{self, MapError, Sv39PageTable},
};
use crate::vm::{AddressSpaceBackend, MappingPermission, PageAlloc, PhysicalRange, VirtualRange};

/// RISC-V 投影：`KernelAddressSpace` 的 Sv39 后端。
pub struct Sv39AddressSpace {
    asid: u16,
    table: Sv39PageTable,
}

impl Sv39AddressSpace {
    /// 用 `alloc`（buddy allocator 的窄接口）构造一个空页表。
    ///
    /// TODO(你)：`let table = Sv39PageTable::new(alloc)?;` 再存 `{ asid, table }`。
    pub fn new(alloc: PageAlloc, asid: u16) -> Result<Self, MapError> {
        todo!("Sv39AddressSpace::new")
    }

    /// 供 `mmu::activate` 写 satp 用的根页表物理页号。
    pub fn root_ppn(&self) -> usize {
        self.table.root_ppn()
    }

    pub fn asid(&self) -> u16 {
        self.asid
    }
}

impl AddressSpaceBackend for Sv39AddressSpace {
    type Error = sv39::MapError;

    /// TODO(你)：`self.table.map_range(va, pa, perm)`。
    fn map(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        perm: MappingPermission,
    ) -> Result<(), Self::Error> {
        todo!("Sv39AddressSpace::map")
    }

    /// TODO(你)：`self.table.unmap_range(va)`。
    fn unmap(&mut self, va: VirtualRange) -> Result<(), Self::Error> {
        todo!("Sv39AddressSpace::unmap")
    }

    /// TODO(你)：`self.table.translate(va)`。
    fn translate(&self, va: usize) -> Option<usize> {
        todo!("Sv39AddressSpace::translate")
    }

    /// TODO(你)：`unsafe { mmu::activate(self.table.root_ppn()) }; Ok(())`。
    /// 切换 satp 的唯一入口；mmu 只负责写寄存器 + sfence。
    fn activate(&self) -> Result<(), Self::Error> {
        todo!("Sv39AddressSpace::activate")
    }
}
