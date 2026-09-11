//! 所有 typed authority table 共用的 slot 生命周期机制。

use super::{Handle, HandleError, Slot};
use crate::component::ComponentId;
use alloc::vec::Vec;

/// 资源 authority 的通用真相表。
///
/// `T` 只表示 slot 中保存的资源对象；不同资源类型仍然通过
/// `Handle<T>` 保持类型隔离。IRQ/MMIO 的资源规则由各自的 wrapper 负责，
/// 这里仅处理 slot、generation、owner 和生命周期。
pub(crate) struct ResourceTable<T> {
    slots: Vec<Slot<T>>,
}

impl<T> ResourceTable<T> {
    pub(crate) const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// 授予 authority；优先复用空 slot，并保留该 slot 当前 generation。
    pub(crate) fn grant(&mut self, owner: ComponentId, object: T) -> Handle<T> {
        if let Some(slot_index) = self.slots.iter().position(|slot| slot.is_vacant()) {
            let slot_id = u32::try_from(slot_index).expect("resource slot index exhausted");
            let slot = &mut self.slots[slot_index];
            let generation = slot.generation();
            slot.activate(owner, object);
            return Handle::new(slot_id, generation);
        }

        let slot_id = u32::try_from(self.slots.len()).expect("resource slot index exhausted");
        let generation = 0;
        self.slots.push(Slot::new(generation, owner, object));
        Handle::new(slot_id, generation)
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得资源对象。
    pub(crate) fn get(
        &self,
        caller: ComponentId,
        handle: Handle<T>,
    ) -> Result<&T, HandleError> {
        let slot = self
            .slots
            .get(handle.slot() as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation() != handle.generation() {
            return Err(HandleError::Stale);
        }
        if slot.owner() != caller {
            return Err(HandleError::WrongOwner);
        }
        slot.object().ok_or(HandleError::Revoked)
    }

    /// 撤销指定组件拥有的全部 authority。
    pub(crate) fn revoke_owner(&mut self, owner: ComponentId) {
        for slot in self.slots.iter_mut() {
            if slot.owner() == owner {
                slot.revoke();
            }
        }
    }

    /// 组件主动释放一个 authority。
    pub(crate) fn release(
        &mut self,
        caller: ComponentId,
        handle: Handle<T>,
    ) -> Result<(), HandleError> {
        let slot = self
            .slots
            .get_mut(handle.slot() as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation() != handle.generation() {
            return Err(HandleError::Stale);
        }
        if slot.owner() != caller {
            return Err(HandleError::WrongOwner);
        }

        slot.revoke();
        Ok(())
    }
}

impl<T> Default for ResourceTable<T> {
    fn default() -> Self {
        Self::new()
    }
}
