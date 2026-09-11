//! IRQ authority 表骨架（C6 实现）。

use super::{Handle, HandleError, Slot};
use crate::component::ComponentId;
use alloc::vec::Vec;

/// IRQ slot 中的资源对象占位。
pub struct Irq {
    _private: (),
}

/// Core 授予组件的 IRQ authority。
pub type IrqHandle = Handle<Irq>;

/// IRQ 资源真相表。
pub struct IrqTable {
    slots: Vec<Slot<Irq>>,
}

impl IrqTable {
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    pub fn grant(&mut self, owner: ComponentId, irq: Irq) -> IrqHandle {
        let _ = (&mut self.slots, owner, irq);
        todo!("C6: grant IRQ authority")
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得 IRQ 对象。
    pub fn get(&self, caller: ComponentId, handle: IrqHandle) -> Result<&Irq, HandleError> {
        let _ = (&self.slots, caller, handle);
        todo!("C6: validate and get IRQ authority")
    }

    pub fn revoke_owner(&mut self, owner: ComponentId) {
        let _ = (&mut self.slots, owner);
        todo!("C6: revoke all IRQ authority owned by component")
    }

    pub fn release(&mut self, caller: ComponentId, handle: IrqHandle) -> Result<(), HandleError> {
        let _ = (&mut self.slots, caller, handle);
        todo!("C6: release IRQ authority")
    }
}

impl Default for IrqTable {
    fn default() -> Self {
        Self::new()
    }
}
