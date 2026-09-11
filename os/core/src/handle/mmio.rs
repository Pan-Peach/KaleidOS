//! MMIO authority 表骨架（C6 实现）。

use super::{Handle, HandleError, ResourceTable};
use crate::component::ComponentId;

/// MMIO slot 中的资源对象占位。
pub struct MmioRegion {
    _private: (),
}

/// Core 授予组件的 MMIO authority。
pub type MmioHandle = Handle<MmioRegion>;

/// MMIO 资源真相表。
pub struct MmioTable {
    table: ResourceTable<MmioRegion>,
}

impl MmioTable {
    pub const fn new() -> Self {
        Self {
            table: ResourceTable::new(),
        }
    }

    pub fn grant(&mut self, owner: ComponentId, region: MmioRegion) -> MmioHandle {
        self.table.grant(owner, region)
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得 MMIO 对象。
    pub fn get(&self, caller: ComponentId, handle: MmioHandle) -> Result<&MmioRegion, HandleError> {
        self.table.get(caller, handle)
    }

    pub fn revoke_owner(&mut self, owner: ComponentId) {
        self.table.revoke_owner(owner)
    }

    pub fn release(&mut self, caller: ComponentId, handle: MmioHandle) -> Result<(), HandleError> {
        self.table.release(caller, handle)
    }
}

impl Default for MmioTable {
    fn default() -> Self {
        Self::new()
    }
}
