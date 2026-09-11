//! IRQ authority 表骨架（C6 实现）。

use super::{Handle, HandleError, ResourceTable};
use crate::component::ComponentId;

/// IRQ slot 中的资源对象占位。
pub struct Irq {
    _private: (),
}

/// Core 授予组件的 IRQ authority。
pub type IrqHandle = Handle<Irq>;

/// IRQ 资源真相表。
pub struct IrqTable {
    table: ResourceTable<Irq>,
}

impl IrqTable {
    pub const fn new() -> Self {
        Self {
            table: ResourceTable::new(),
        }
    }

    /// 授予组件 IRQ authority。
    pub fn grant(&mut self, owner: ComponentId, irq: Irq) -> IrqHandle {
        self.table.grant(owner, irq)
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得 IRQ 对象。
    pub fn get(&self, caller: ComponentId, handle: IrqHandle) -> Result<&Irq, HandleError> {
        self.table.get(caller, handle)
    }

    pub fn revoke_owner(&mut self, owner: ComponentId) {
        self.table.revoke_owner(owner);
    }

    pub fn release(&mut self, caller: ComponentId, handle: IrqHandle) -> Result<(), HandleError> {
        self.table.release(caller, handle)
    }
}

impl Default for IrqTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Irq, IrqTable};
    use crate::component::ComponentId;
    use crate::handle::HandleError;

    fn irq() -> Irq {
        Irq { _private: () }
    }

    #[test]
    fn grant_reuses_vacant_slot_with_new_generation() {
        let owner_a = ComponentId::from_raw(1);
        let owner_b = ComponentId::from_raw(2);
        let mut table = IrqTable::new();

        let old = table.grant(owner_a, irq());
        table.revoke_owner(owner_a);
        let new = table.grant(owner_b, irq());

        assert_eq!(old.slot(), new.slot());
        assert_ne!(old.generation(), new.generation());
        assert!(matches!(table.get(owner_a, old), Err(HandleError::Stale)));
        assert!(table.get(owner_b, new).is_ok());
    }

    #[test]
    fn revoke_owner_revokes_all_owned_slots() {
        let owner_a = ComponentId::from_raw(1);
        let owner_b = ComponentId::from_raw(2);
        let mut table = IrqTable::new();

        let first = table.grant(owner_a, irq());
        let second = table.grant(owner_a, irq());
        let other = table.grant(owner_b, irq());

        table.revoke_owner(owner_a);

        assert!(matches!(table.get(owner_a, first), Err(HandleError::Stale)));
        assert!(matches!(
            table.get(owner_a, second),
            Err(HandleError::Stale)
        ));
        assert!(table.get(owner_b, other).is_ok());
    }

    #[test]
    fn release_only_releases_the_exact_handle() {
        let owner = ComponentId::from_raw(1);
        let other = ComponentId::from_raw(2);
        let mut table = IrqTable::new();

        let first = table.grant(owner, irq());
        let second = table.grant(owner, irq());

        assert_eq!(table.release(other, first), Err(HandleError::WrongOwner));
        assert!(table.get(owner, first).is_ok());

        assert_eq!(table.release(owner, second), Ok(()));
        assert!(table.get(owner, first).is_ok());
        assert!(matches!(table.get(owner, second), Err(HandleError::Stale)));
        assert_eq!(table.release(owner, second), Err(HandleError::Stale));
    }
}
