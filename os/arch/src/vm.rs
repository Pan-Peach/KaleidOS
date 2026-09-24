//! Architecture-neutral VM backend contract.
//!
//! Core 的 `KernelAddressSpace`（os/core/src/memory/address_space.rs）直接消费
//! 这里的 `AddressSpaceBackend` / `VirtualRange` / `PhysicalRange` /
//! `MappingPermission`；`PageAlloc` 是 core buddy heap 给页表 backend 的窄回调。
//! 未来的 contract crate 可以把这些类型抽出来，让 Core 支持多种翻译 backend，
//! 但目前保持在本 crate 内即可。

use bitflags::bitflags;

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

// 逻辑权限：正交的位集合（arch 无关）。默认为空 = 无权限。
// 到具体 arch 的编码（如 Sv39/Sv32 的 R/W/X/U）由各后端在 `From` 里翻译。
bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
    pub struct MappingPermission: u8 {
        const READ    = 1 << 0;
        const WRITE   = 1 << 1;
        const EXECUTE = 1 << 2;
        const USER    = 1 << 3;
    }
}

/// 一次性"给一个已归零的页，返回其物理地址"的钩子。
///
/// 这就是 buddy allocator（`core::memory::alloc_region`）的窄接口：
/// 因为 `os/arch` 不能依赖 `os/core`，Core 在初始化时把这个函数地址塞进
/// 当前 RISC-V 页表 backend。v1 identity 阶段返回的物理地址可直接当虚拟地址解引用。
pub type PageAlloc = fn() -> Result<usize, ()>;

/// Contract `KernelAddressSpace` drives. Methods take raw ranges/permissions;
/// Core validates & commits around the call. The active architecture backend
/// supplies the concrete implementation.
pub trait AddressSpaceBackend {
    /// 该后端要求映射区间满足的对齐/步进粒度（必须是 2 的幂）。
    ///
    /// Core 的地址空间校验用它做对齐检查，**不引用任何分配器常量**：
    /// Sv39/Sv32 = `VM_PAGE_SIZE`（4 KiB）；NoMMU = 1（恒等，无对齐约束，
    /// 校验自动退化为 no-op）。这保证 Core 不偷偷依赖"必须有 MMU 页"。
    const GRANULE: usize;

    type Error;

    fn create(alloc: PageAlloc) -> Result<Self, Self::Error>
    where
        Self: Sized;

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
