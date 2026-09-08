//! Architecture-neutral VM backend contract skeleton.
//!
//! This is vocabulary only. It is not wired into Core or the boot path yet.
//! A future contract crate may extract these types when C10 needs multiple
//! translation backends without making Core depend on a concrete Arch crate.

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
pub struct MappingPermission {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub user: bool,
}

pub trait PageTablePageAllocator {
    type Page;
    type Error;

    fn allocate_zeroed(&mut self) -> Result<Self::Page, Self::Error>;
    fn release(&mut self, page: Self::Page) -> Result<(), Self::Error>;
}

pub trait AddressTranslationBackend {
    type Space;
    type Error;

    fn create_space(&mut self) -> Result<Self::Space, Self::Error>;
    fn map_range(
        &mut self,
        space: &mut Self::Space,
        virtual_range: VirtualRange,
        physical_range: PhysicalRange,
        permission: MappingPermission,
    ) -> Result<(), Self::Error>;
    fn unmap_range(
        &mut self,
        space: &mut Self::Space,
        virtual_range: VirtualRange,
    ) -> Result<(), Self::Error>;
    fn activate(&mut self, space: &Self::Space) -> Result<(), Self::Error>;
    fn destroy_space(&mut self, space: Self::Space) -> Result<(), Self::Error>;
}
