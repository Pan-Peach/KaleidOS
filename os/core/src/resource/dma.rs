//! DMA：**allocation 与 mapping 分离**的 Core 机制。
//!
//! # 两个独立概念
//!
//! ```text
//! allocation（device-agnostic）      mapping（device-related）
//!   kcore_dma_alloc(size)              kcore_dma_map(device_id, ptr, len, dir)
//!     → CPU-visible buffer (ptr,len)     → device-visible address + mapping id
//!   kcore_dma_free(ptr)                kcore_dma_unmap(mapping)
//! ```
//!
//! 分配后端只负责"给我一块满足约束的物理连续内存"，**不知道** VirtIO / NVMe /
//! NIC，也不知道具体 `DeviceId`。映射才知道设备：No-IOMMU 时 device address 就是
//! 物理地址（identity）；IOMMU 只需在 [`map`] 内把 buffer PA → IOVA，或当设备
//! 地址受限时经 bounce buffer，**上层 driver 不变**。分配**不**依赖
//! `MmioHandle`：把分配 + 设备身份证明 + 映射绑在一起是错误的耦合，`alloc`
//! 必须 device-agnostic。
//!
//! # quarantine：DMA lifecycle safety（correctness，不是 security）
//!
//! ```text
//! 组件失败 / 显式 free  ≠  设备已静默
//! ```
//!
//! 设备可能仍在 DMA 往这块内存写；立即 free/复用会让设备写进已被重新分配的
//! 区域。无 IOMMU 时 Core **无法确认设备已静默**，因此 backing lease 一律 move
//! 进 Core 私有 `QUARANTINE`（不归还 buddy）。权威回收需要设备静默
//! （reset / 确认无未结清），当前没有该机制。
//!
//! # DmaMappingId：真实动态生命周期
//!
//! mapping 是真实的长生命周期对象（map → 设备使用一段 → unmap），且会复用。
//! 这里用**单调递增 id**（从不复用）：stale id 自然查不到，无需 generational
//! slot。这是唯一保留"id"的对象，因为它对应真实生命周期，不是为统一抽象而设。

use super::{RequestContext, ResourceKind};
use crate::component::ComponentId;
use crate::irq::IrqSaveGuard;
use crate::machine::{self, DeviceId};
use crate::memory::{self, MemoryError, MemoryLease};
use crate::trace::{TraceEvent, emit};
use alloc::vec::Vec;
use spin::{Mutex, Once};

/// DMA 传输方向。**ABI 编码 0/1/2**（与 SDK `DmaDirection` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaDirection {
    /// 内存 → 设备。
    ToDevice,
    /// 设备 → 内存。
    FromDevice,
    /// 双向。
    Bidirectional,
}

impl DmaDirection {
    /// ABI 编码（`kcore_dma_map` 的 `direction` 参数）。
    pub const fn as_i32(self) -> i32 {
        match self {
            DmaDirection::ToDevice => 0,
            DmaDirection::FromDevice => 1,
            DmaDirection::Bidirectional => 2,
        }
    }

    /// ABI 解码。
    pub fn from_i32(direction: i32) -> Option<Self> {
        match direction {
            0 => Some(DmaDirection::ToDevice),
            1 => Some(DmaDirection::FromDevice),
            2 => Some(DmaDirection::Bidirectional),
            _ => None,
        }
    }
}

/// allocation / mapping 失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// 请求尺寸非法（0 / 超出分配器上限）。
    InvalidSize,
    /// 物理内存耗尽。
    Exhausted,
    /// `DeviceId` 越界 / 机器信息未提交。
    DeviceNotFound,
    /// caller 不是该 device / allocation / mapping 的 owner。
    NotOwner,
    /// 目标 allocation / mapping 不存在。
    NotFound,
    /// `ptr + len` 溢出或 `len == 0`。
    BadRange,
}

/// 一次 device 可见地址的映射结果。
#[derive(Debug, Clone, Copy)]
pub struct DmaMapping {
    /// 设备可见地址（No-IOMMU：identity == buffer 物理/内核地址）。
    pub device_addr: usize,
    /// mapping identity（unmap 用）。
    pub id: u64,
}

/// 一次 CPU-visible DMA 缓冲分配结果。
#[derive(Debug, Clone, Copy)]
pub struct DmaBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}

/// 一条 allocation 的真相。
struct Allocation {
    base: usize,
    owner: ComponentId,
    /// backing 租约；`None` = 已 quarantine。
    lease: Option<MemoryLease>,
}

/// 一条 mapping 的真相。
struct Mapping {
    id: u64,
    owner: ComponentId,
    device: DeviceId,
}

/// DMA allocation + mapping 真相表。
pub struct DmaTable {
    allocations: Vec<Allocation>,
    mappings: Vec<Mapping>,
    next_id: u64,
}

impl DmaTable {
    pub const fn new() -> Self {
        Self {
            allocations: Vec::new(),
            mappings: Vec::new(),
            next_id: 1,
        }
    }

    /// 登记一段新分配（调用方已完成 `memory::alloc_region`）。
    fn insert_allocation(&mut self, owner: ComponentId, lease: MemoryLease) -> DmaBuffer {
        let buffer = DmaBuffer {
            ptr: lease.base() as *mut u8,
            len: lease.size(),
        };
        self.allocations.push(Allocation {
            base: lease.base(),
            owner,
            lease: Some(lease),
        });
        buffer
    }

    /// 释放 allocation：backing lease 移入 quarantine（**不 free**）。
    fn release_allocation(&mut self, owner: ComponentId, ptr: usize) -> Result<(), DmaError> {
        let Some(index) = self
            .allocations
            .iter()
            .position(|allocation| allocation.base == ptr)
        else {
            return Err(DmaError::NotFound);
        };
        if self.allocations[index].owner != owner {
            return Err(DmaError::NotOwner);
        }
        if let Some(lease) = self.allocations[index].lease.take() {
            quarantine(lease);
        }
        self.allocations.swap_remove(index);
        Ok(())
    }

    /// 登记一条映射：device_addr（No-IOMMU identity）与单调 id。
    fn insert_mapping(
        &mut self,
        owner: ComponentId,
        device: DeviceId,
        ptr: usize,
        _len: usize,
    ) -> DmaMapping {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let device_addr = ptr; // No-IOMMU identity；IOMMU 时改为 IOVA。
        self.mappings.push(Mapping { id, owner, device });
        DmaMapping { device_addr, id }
    }

    /// 撤销一条映射（按 id 唯一定位）。
    ///
    /// **不校验 ambient owner**：映射归属 Core truth = **device owner**，而映射的
    /// map/unmap 可能发生在 consumer 的任务上下文（provider 方法被直接调用，
    /// `RequestContext::ambient()` 解析为 consumer）。KernelNative 是协作式信任，
    /// 这里不伪造鉴权；id 从不复用，stale id 自然查不到。
    fn remove_mapping(&mut self, id: u64) -> Result<ComponentId, DmaError> {
        let Some(index) = self.mappings.iter().position(|mapping| mapping.id == id) else {
            return Err(DmaError::NotFound);
        };
        let owner = self.mappings[index].owner;
        self.mappings.swap_remove(index);
        Ok(owner)
    }

    /// 该设备上是否还有 live mapping（供 `device::release` 的子项检查）。
    pub fn has_mapping_for_device(&self, device: DeviceId) -> bool {
        self.mappings.iter().any(|mapping| mapping.device == device)
    }

    /// 撤销 owner 的全部映射，并把其全部 allocation backing quarantine。
    pub fn revoke_owner(&mut self, owner: ComponentId) {
        self.mappings.retain(|mapping| mapping.owner != owner);
        for allocation in self.allocations.iter_mut() {
            if allocation.owner == owner
                && let Some(lease) = allocation.lease.take()
            {
                quarantine(lease);
            }
        }
        self.allocations
            .retain(|allocation| allocation.owner != owner);
    }
}

impl Default for DmaTable {
    fn default() -> Self {
        Self::new()
    }
}

// —— 全局（`resource::init` 初始化；测试用 `DmaTable::new()`）——

static TABLE: Once<Mutex<DmaTable>> = Once::new();

/// Core 私有 quarantine：已释放 / 已撤销但设备可能仍在 DMA 的 backing 区域。
///
/// 只增不减：本版没有"确认设备静默"的手段，因此不归还、不复用。
static QUARANTINE: Mutex<Vec<MemoryLease>> = Mutex::new(Vec::new());

fn quarantine(lease: MemoryLease) {
    QUARANTINE.lock().push(lease);
}

/// 已 quarantine 的区域数（测试断言"未释放"用）。
#[cfg(test)]
pub(crate) fn quarantine_len() -> usize {
    QUARANTINE.lock().len()
}

/// 初始化全局 DMA 表（`resource::init` 调用一次）。
pub fn init() {
    TABLE.call_once(|| Mutex::new(DmaTable::new()));
}

/// 取全局 DMA 表（init 后可用）。
pub fn get_table() -> &'static Mutex<DmaTable> {
    TABLE.get().expect("dma table not initialized")
}

/// 该设备上是否还有 live DMA mapping（供 `device::release` 的子项检查）。
pub fn has_mapping_for_device(device: DeviceId) -> bool {
    get_table().lock().has_mapping_for_device(device)
}

/// 分配一段物理连续的 DMA 缓冲（device-agnostic）。
pub fn alloc(owner: ComponentId, size: usize) -> Result<DmaBuffer, DmaError> {
    let _guard = IrqSaveGuard::new();
    // 先在分配器锁下取内存（不持 DmaTable 锁分配，避免跨锁）。
    let lease = memory::alloc_region(size).map_err(|error| match error {
        MemoryError::Exhausted => DmaError::Exhausted,
        MemoryError::InvalidSize | MemoryError::DoubleFree => DmaError::InvalidSize,
    })?;
    Ok(get_table().lock().insert_allocation(owner, lease))
}

/// 释放一段 DMA 缓冲：backing lease 进 quarantine（**不 free**，见模块文档）。
pub fn free(owner: ComponentId, ptr: *mut u8) -> Result<(), DmaError> {
    if ptr.is_null() {
        return Err(DmaError::BadRange);
    }
    let _guard = IrqSaveGuard::new();
    get_table().lock().release_allocation(owner, ptr as usize)
}

/// 把 buffer 映射给某台设备：返回设备可见地址 + mapping id。
///
/// **归属 Core truth = device owner**（不是 ambient caller）：provider 方法可能在
/// consumer 的任务上下文执行，映射仍应记在设备 owner 名下，才能在 owner 失败/
/// 卸载时被正确回收。设备未被认领 → `NotOwner`。
pub fn map(
    ctx: &RequestContext,
    device: DeviceId,
    ptr: *mut u8,
    len: usize,
    direction: DmaDirection,
) -> Result<DmaMapping, DmaError> {
    let _ = (ctx, direction); // ctx 仅为 ABI 一致性；方向对 No-IOMMU identity 无影响。
    if ptr.is_null() || len == 0 || (ptr as usize).checked_add(len).is_none() {
        return Err(DmaError::BadRange);
    }
    let Some(machine) = machine::committed() else {
        return Err(DmaError::DeviceNotFound);
    };
    if machine.devices.get(device.raw() as usize).is_none() {
        return Err(DmaError::DeviceNotFound);
    }

    let _guard = IrqSaveGuard::new();
    // 锁序 device → dma（device 表是最外层）。
    let device_table = super::device::get_table().lock();
    let Some(owner) = device_table.owner(device) else {
        return Err(DmaError::NotOwner);
    };
    let mapping = get_table()
        .lock()
        .insert_mapping(owner, device, ptr as usize, len);
    // device release checks child mappings under this same device lock.
    // Keep ownership stable until the new child is visible.
    drop(device_table);
    emit(TraceEvent::ResourceGrant {
        component: owner,
        kind: ResourceKind::Dma,
        id: mapping.id,
    });
    Ok(mapping)
}

/// 撤销一条 mapping（按 id；归属记录在 mapping 上）。
pub fn unmap(id: u64) -> Result<(), DmaError> {
    let _guard = IrqSaveGuard::new();
    let owner = get_table().lock().remove_mapping(id)?;
    emit(TraceEvent::ResourceRevoke {
        component: owner,
        kind: ResourceKind::Dma,
        id,
    });
    Ok(())
}

/// 撤销 owner 的全部 DMA mapping + allocation（失败路径；backing 进 quarantine）。
pub fn revoke_owner(owner: ComponentId) {
    let _guard = IrqSaveGuard::new();
    get_table().lock().revoke_owner(owner);
}

#[cfg(test)]
mod tests {
    use super::{DmaDirection, DmaError, DmaTable};
    use crate::component::ComponentId;
    use crate::machine::DeviceId;
    use crate::memory::test_support;

    fn cid(raw: u32) -> ComponentId {
        ComponentId::from_raw(raw)
    }

    #[test]
    fn direction_encoding_roundtrips() {
        for direction in [
            DmaDirection::ToDevice,
            DmaDirection::FromDevice,
            DmaDirection::Bidirectional,
        ] {
            assert_eq!(DmaDirection::from_i32(direction.as_i32()), Some(direction));
        }
        assert_eq!(DmaDirection::from_i32(3), None);
    }

    #[test]
    fn mapping_ids_are_monotonic_and_removed_mapping_is_not_found() {
        let owner = cid(1);
        let device = DeviceId::from_raw(0);
        let mut table = DmaTable::new();
        let first = table.insert_mapping(owner, device, 0x1000, 16);
        let second = table.insert_mapping(owner, device, 0x2000, 16);
        assert_ne!(first.id, second.id, "mapping id 单调唯一");
        assert_eq!(first.device_addr, 0x1000, "No-IOMMU identity");

        assert_eq!(table.remove_mapping(first.id), Ok(cid(1)));
        // 已移除的 id 查不到（无 ABA：id 从不复用）。
        assert_eq!(table.remove_mapping(first.id), Err(DmaError::NotFound));
        assert!(table.has_mapping_for_device(device));
        assert_eq!(table.remove_mapping(second.id), Ok(cid(1)));
        assert!(!table.has_mapping_for_device(device));
    }

    /// 设备身份全宽：mapping 记的是 `DeviceId`（≥ 256 也成立），无 u8 收窄。
    #[test]
    fn mapping_tracks_full_width_device_identity() {
        let owner = cid(1);
        let far = DeviceId::from_raw(260);
        let mut table = DmaTable::new();
        let mapping = table.insert_mapping(owner, far, 0x1000, 16);
        assert!(table.has_mapping_for_device(far));
        assert!(!table.has_mapping_for_device(DeviceId::from_raw(0)));
        assert_eq!(table.remove_mapping(mapping.id), Ok(owner));
    }

    /// 未注册的 mapping id → NotFound（从不复用 → 无 ABA 误命中）。
    #[test]
    fn unmap_unknown_id_is_not_found() {
        let mut table = DmaTable::new();
        let mapping = table.insert_mapping(cid(1), DeviceId::from_raw(0), 0x1000, 16);
        assert_eq!(
            table.remove_mapping(mapping.id + 1),
            Err(DmaError::NotFound)
        );
        assert_eq!(table.remove_mapping(mapping.id), Ok(cid(1)));
        assert_eq!(table.remove_mapping(mapping.id), Err(DmaError::NotFound));
    }

    #[test]
    fn alloc_free_quarantines_backing_and_never_returns_to_heap() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();
        super::init();

        let owner = cid(50);
        let buffer = super::alloc(owner, 4096).expect("alloc");
        assert!(!buffer.ptr.is_null());
        assert!(buffer.len >= 4096);

        let before_quarantine = super::quarantine_len();
        let before_free = crate::memory::free_block_counts();

        assert_eq!(super::free(owner, buffer.ptr), Ok(()));

        assert_eq!(
            super::quarantine_len(),
            before_quarantine + 1,
            "backing 必须进 quarantine"
        );
        assert_eq!(
            crate::memory::free_block_counts(),
            before_free,
            "quarantine 的 backing 不得归还 buddy heap"
        );
        // 非 owner 不能释放别人的 allocation。
        let other_buffer = super::alloc(owner, 4096).expect("alloc 2");
        assert_eq!(
            super::free(cid(51), other_buffer.ptr),
            Err(DmaError::NotOwner)
        );
    }

    #[test]
    fn revoke_owner_quarantines_allocations_and_drops_mappings() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();
        super::init();

        let owner = cid(52);
        let buffer = super::alloc(owner, 8192).expect("alloc");
        super::get_table().lock().insert_mapping(
            owner,
            DeviceId::from_raw(3),
            buffer.ptr as usize,
            buffer.len,
        );

        let before_quarantine = super::quarantine_len();
        let before_free = crate::memory::free_block_counts();

        super::revoke_owner(owner);

        assert_eq!(super::quarantine_len(), before_quarantine + 1);
        assert_eq!(crate::memory::free_block_counts(), before_free);
        assert!(
            !super::get_table()
                .lock()
                .has_mapping_for_device(DeviceId::from_raw(3))
        );
    }
}
