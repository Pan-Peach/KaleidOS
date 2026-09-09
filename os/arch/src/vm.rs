//! Architecture-neutral VM backend contract skeleton.
//!
//! This is vocabulary only. It is not wired into Core or the boot path yet.
//! A future contract crate may extract these types when C10 needs multiple
//! translation backends without making Core depend on a concrete architecture
//! implementation.

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

/// 一次性"给一个已归零的页，返回其物理地址"的钩子。
///
/// 这就是 buddy allocator（`core::memory::alloc_region`）的窄接口：
/// 因为 `os/arch` 不能依赖 `os/core`，Core 在初始化时把这个函数地址塞进
/// `Sv39PageTable`。v1 identity 阶段返回的物理地址可直接当虚拟地址解引用。
pub type PageAlloc = fn() -> Result<usize, ()>;

/// Contract `KernelAddressSpace` drives. Methods take raw ranges/permissions;
/// Core validates & commits around the call. You fill in the `Sv39AddressSpace`
/// implementation.
pub trait AddressSpaceBackend {
    type Error;

    fn map(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        perm: MappingPermission,
    ) -> Result<(), Self::Error>;
    fn unmap(&mut self, va: VirtualRange) -> Result<(), Self::Error>;
    fn translate(&self, va: usize) -> Option<usize>;
    fn activate(&self) -> Result<(), Self::Error>;
}
