//! Runtime address-space vocabulary and ownership skeleton.
//!
//! This module intentionally contains semantic Core state only. The concrete
//! translation representation belongs to an architecture backend.

use crate::component::ComponentId;
use crate::memory::PAGE_SIZE;

// 共享词汇表直接复用 arch::vm（os/core 依赖 os/arch，方向正确）。
// 这里 re-export 一份，让 `address_space::PhysicalRange` 等对 memory/mod.rs 仍可用。
pub use arch::vm::{AddressSpaceBackend, MappingPermission, PhysicalRange, VirtualRange};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub virtual_range: VirtualRange,
    pub physical_range: PhysicalRange,
    pub permission: MappingPermission,
}

/// Core 把已批准的映射翻译成 arch backend 的原始参数形式。
/// 现在 `Mapping` 字段本身就是 arch 类型，所以直接透传即可。
impl Mapping {
    pub(crate) fn to_backend(&self) -> (VirtualRange, PhysicalRange, MappingPermission) {
        (self.virtual_range, self.physical_range, self.permission)
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

    /// Core 校验一次映射：非空、等长、页对齐、不重叠。只接受批准后的映射。
    ///
    /// - `virtual_range.size == 0 || physical_range.size == 0` -> `EmptyRange`
    /// - `virtual_range.size != physical_range.size`           -> `LengthMismatch`
    /// - `base` 或 `size` 未页对齐                               -> `Unaligned`
    /// - 与任一已有 mapping 的虚拟区间重叠                       -> `Overlap`
    ///
    /// 物理区重叠不查：同一物理页映射到多个 VA 是合法的（别名映射）。
    fn validate(&self, mapping: &Mapping) -> Result<(), MapError> {
        let vr = mapping.virtual_range;
        let pr = mapping.physical_range;
        if vr.size == 0 || pr.size == 0 {
            return Err(MapError::EmptyRange);
        }
        if vr.size != pr.size {
            return Err(MapError::LengthMismatch);
        }
        if !is_page_aligned(vr.base) || !is_page_aligned(pr.base) {
            return Err(MapError::Unaligned);
        }
        if !is_page_aligned(vr.size) || !is_page_aligned(pr.size) {
            return Err(MapError::Unaligned);
        }
        for m in &self.mappings {
            if ranges_overlap(&m.virtual_range, &vr) {
                return Err(MapError::Overlap);
            }
        }
        Ok(())
    }

    /// 映射流程：1) Core 验证  2) 后端写 PTE  3) Core 记录真相。
    ///
    /// 后端写 PTE 失败即整体失败，不 record 到 `mappings`（后端内部负责回滚）。
    pub fn map(&mut self, mapping: Mapping) -> Result<(), MapError> {
        self.validate(&mapping)?;
        let (va, pa, perm) = mapping.to_backend();
        self.backend
            .map(va, pa, perm)
            .map_err(|_| MapError::BackendFailed)?;
        self.commit(mapping);
        Ok(())
    }

    /// 把已批准并落地的映射写入真相列表。
    fn commit(&mut self, mapping: Mapping) {
        self.mappings.push(mapping);
    }

    /// 解除映射：后端清 PTE，Core 从 `mappings` 里移除重叠项。
    ///
    /// 后端 `unmap` 失败则整体失败，不触碰真相列表。
    pub fn unmap(&mut self, range: &VirtualRange) -> Result<(), MapError> {
        self.backend
            .unmap(*range)
            .map_err(|_| MapError::BackendFailed)?;
        self.mappings
            .retain(|m| !ranges_overlap(&m.virtual_range, range));
        Ok(())
    }

    /// 把虚拟地址翻译成物理地址，委托给后端。
    pub fn translate(&self, va: usize) -> Option<usize> {
        self.backend.translate(va)
    }

    /// 把本地址空间激活为当前 satp（委托后端写 satp + sfence）。
    pub fn activate(&self) -> Result<(), MapError> {
        self.backend.activate().map_err(|_| MapError::BackendFailed)
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
