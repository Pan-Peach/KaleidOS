//! Runtime address-space vocabulary and ownership skeleton.
//!
//! This module intentionally contains semantic Core state only. The concrete
//! translation representation belongs to an architecture backend.

use crate::component::ComponentId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AddressSpaceId(u32);

impl AddressSpaceId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AddressSpaceHandle {
    id: AddressSpaceId,
    generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressSpaceState {
    Ready,
    Dying,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VirtualRange {
    pub base: usize,
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PhysicalRange {
    pub base: usize,
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MemoryPermission {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub user: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub virtual_range: VirtualRange,
    pub physical_range: PhysicalRange,
    pub permission: MemoryPermission,
}

/// Core-owned semantic record. `ArchSpace` is opaque to this module.
/// Backend installation and lifecycle operations will be added at C10.
pub struct AddressSpaceSlot<ArchSpace> {
    pub generation: u32,
    pub owner: ComponentId,
    pub state: AddressSpaceState,
    pub mappings: alloc::vec::Vec<Mapping>,
    pub arch: ArchSpace,
}

/// Future Core authority boundary for create/map/unmap/activate/destroy.
pub struct AddressSpaceManager<ArchSpace> {
    pub slots: alloc::vec::Vec<AddressSpaceSlot<ArchSpace>>,
}

impl<ArchSpace> AddressSpaceManager<ArchSpace> {
    pub const fn empty() -> Self {
        Self {
            slots: alloc::vec::Vec::new(),
        }
    }
}

#[allow(dead_code)]
const fn _handle_shape(id: AddressSpaceId, generation: u32) -> AddressSpaceHandle {
    AddressSpaceHandle { id, generation }
}
