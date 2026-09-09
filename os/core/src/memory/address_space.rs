//! Runtime address-space vocabulary and ownership skeleton.
//!
//! This module intentionally contains semantic Core state only. The concrete
//! translation representation belongs to an architecture backend.

use crate::component::ComponentId;
use crate::memory::PAGE_SIZE;
use arch::vm::{AddressSpaceBackend, MappingPermission};

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

/// Core 把已批准的映射翻译成 arch backend 的原始参数形式。
impl Mapping {
    pub(crate) fn to_backend(
        &self,
    ) -> (arch::vm::VirtualRange, arch::vm::PhysicalRange, MappingPermission) {
        (
            arch::vm::VirtualRange {
                base: self.virtual_range.base,
                size: self.virtual_range.size,
            },
            arch::vm::PhysicalRange {
                base: self.physical_range.base,
                size: self.physical_range.size,
            },
            MappingPermission {
                read: self.permission.read,
                write: self.permission.write,
                execute: self.permission.execute,
                user: self.permission.user,
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    EmptyRange,
    LengthMismatch,
    Unaligned,
    Overlap,
    BackendFailed,
}

fn is_page_aligned(addr: usize) -> bool {
    addr & (PAGE_SIZE - 1) == 0
}

fn ranges_overlap(a: &VirtualRange, b: &VirtualRange) -> bool {
    a.base < b.base + b.size && b.base < a.base + a.size
}

/// Core-owned logical address space: 资源/权限/生命周期对象。
///
/// 持有真相（state / owner / mapping 列表）与一个不透明的架构后端 `B`。
/// 映射流程：Core `validate` -> backend 写 PTE -> Core `commit` 记录真相。
pub struct KernelAddressSpace<B: AddressSpaceBackend> {
    pub id: AddressSpaceId,
    pub generation: u32,
    pub owner: ComponentId,
    pub state: AddressSpaceState,
    pub mappings: alloc::vec::Vec<Mapping>,
    pub backend: B,
}

impl<B: AddressSpaceBackend> KernelAddressSpace<B> {
    pub fn new(id: AddressSpaceId, generation: u32, owner: ComponentId, backend: B) -> Self {
        Self {
            id,
            generation,
            owner,
            state: AddressSpaceState::Ready,
            mappings: alloc::vec::Vec::new(),
            backend,
        }
    }

    pub fn handle(&self) -> AddressSpaceHandle {
        AddressSpaceHandle {
            id: self.id,
            generation: self.generation,
        }
    }

    /// Core 校验：非空、等长、页对齐、不重叠。
    ///
    /// TODO(你)：按 `MapError` 变体逐条检查。
    ///   - `virtual_range.size == 0 || physical_range.size == 0` -> `EmptyRange`
    ///   - `virtual_range.size != physical_range.size`          -> `LengthMismatch`
    ///   - `is_page_aligned(base)` 三者                           -> `Unaligned`
    ///   - 任一已有 mapping 与 `mapping.virtual_range` 重叠        -> `Overlap`
    fn validate(&self, mapping: &Mapping) -> Result<(), MapError> {
        let _ = (mapping, ranges_overlap, is_page_aligned);
        todo!("KernelAddressSpace::validate")
    }

    /// 1) Core 验证  2) Sv39PageTable 写 PTE  3) Core 记录真相。
    ///
    /// TODO(你)：
    ///   - `self.validate(&mapping)?;`
    ///   - `let (va, pa, perm) = mapping.to_backend();`
    ///   - `self.backend.map(va, pa, perm).map_err(|_| MapError::BackendFailed)?;`
    ///   - `self.commit(mapping);`
    ///   - `Ok(())`
    pub fn map(&mut self, mapping: Mapping) -> Result<(), MapError> {
        todo!("KernelAddressSpace::map")
    }

    fn commit(&mut self, mapping: Mapping) {
        self.mappings.push(mapping);
    }

    /// TODO(你)：`self.backend.unmap(...)` 失败转 `BackendFailed`；成功后从
    /// `mappings` 里 `retain` 掉与 `range` 重叠的项。
    pub fn unmap(&mut self, range: &VirtualRange) -> Result<(), MapError> {
        let _ = range;
        todo!("KernelAddressSpace::unmap")
    }

    /// TODO(你)：`self.backend.translate(va)`。
    pub fn translate(&self, va: usize) -> Option<usize> {
        let _ = va;
        todo!("KernelAddressSpace::translate")
    }

    /// TODO(你)：`self.backend.activate().map_err(|_| MapError::BackendFailed)`。
    pub fn activate(&self) -> Result<(), MapError> {
        todo!("KernelAddressSpace::activate")
    }
}

/// Core authority boundary for create/get/get_mut。
pub struct AddressSpaceManager<B: AddressSpaceBackend> {
    pub spaces: alloc::vec::Vec<KernelAddressSpace<B>>,
    next_id: u32,
}

impl<B: AddressSpaceBackend> AddressSpaceManager<B> {
    pub const fn empty() -> Self {
        Self {
            spaces: alloc::vec::Vec::new(),
            next_id: 0,
        }
    }

    pub fn create(&mut self, owner: ComponentId, backend: B) -> AddressSpaceHandle {
        let id = AddressSpaceId::from_raw(self.next_id);
        self.next_id += 1;
        self.spaces
            .push(KernelAddressSpace::new(id, 1, owner, backend));
        self.spaces.last().unwrap().handle()
    }

    pub fn get(&self, handle: AddressSpaceHandle) -> Option<&KernelAddressSpace<B>> {
        self.spaces
            .iter()
            .find(|s| s.id == handle.id && s.generation == handle.generation)
    }

    pub fn get_mut(&mut self, handle: AddressSpaceHandle) -> Option<&mut KernelAddressSpace<B>> {
        self.spaces
            .iter_mut()
            .find(|s| s.id == handle.id && s.generation == handle.generation)
    }
}

#[allow(dead_code)]
const fn _handle_shape(id: AddressSpaceId, generation: u32) -> AddressSpaceHandle {
    AddressSpaceHandle { id, generation }
}
