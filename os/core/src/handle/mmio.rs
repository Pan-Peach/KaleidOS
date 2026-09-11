//! MMIO authority 表骨架（C6 实现）。

use super::{Handle, HandleError, Slot};
use crate::component::ComponentId;
use alloc::vec::Vec;

/// MMIO slot 中的资源对象占位。
pub struct MmioRegion {
    _private: (),
}

/// Core 授予组件的 MMIO authority。
pub type MmioHandle = Handle<MmioRegion>;

/// MMIO 资源真相表。
pub struct MmioTable {
    slots: Vec<Slot<MmioRegion>>,
}

impl MmioTable {
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    pub fn grant(&mut self, owner: ComponentId, region: MmioRegion) -> MmioHandle {
        let _ = (&mut self.slots, owner, region);
        todo!("C6: grant MMIO authority")
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得 MMIO 对象。
    pub fn get(&self, caller: ComponentId, handle: MmioHandle) -> Result<&MmioRegion, HandleError> {
        let _ = (&self.slots, caller, handle);
        todo!("C6: validate and get MMIO authority")
    }

    pub fn revoke_owner(&mut self, owner: ComponentId) {
        let _ = (&mut self.slots, owner);
        todo!("C6: revoke all MMIO authority owned by component")
    }

    pub fn release(&mut self, caller: ComponentId, handle: MmioHandle) -> Result<(), HandleError> {
        let _ = (&mut self.slots, caller, handle);
        todo!("C6: release MMIO authority")
    }
}

impl Default for MmioTable {
    fn default() -> Self {
        Self::new()
    }
}
