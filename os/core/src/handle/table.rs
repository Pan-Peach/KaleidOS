//! 所有 typed authority table 共用的 slot 生命周期机制。

use super::{Handle, HandleError, RawHandle, ResourceKind, Slot};
use crate::component::ComponentId;
use crate::trace::{TraceEvent, emit};
use alloc::vec::Vec;

/// 资源 authority 的通用真相表。
///
/// `T` 只表示 slot 中保存的资源对象；不同资源类型仍然通过
/// `Handle<T>` 保持类型隔离。IRQ/MMIO 的资源规则由各自的 wrapper 负责，
/// 这里仅处理 slot、generation、owner 和生命周期。
///
/// `kind` **只用于 trace**（grant/revoke 事件要标明哪一类 authority），
/// 不参与任何验证判定，因此不会成为"第二真相"。
pub(crate) struct ResourceTable<T> {
    slots: Vec<Slot<T>>,
    kind: ResourceKind,
}

impl<T> ResourceTable<T> {
    pub(crate) const fn new(kind: ResourceKind) -> Self {
        Self {
            slots: Vec::new(),
            kind,
        }
    }

    /// 授予 authority；优先复用空 slot，并保留该 slot 当前 generation。
    pub(crate) fn grant(&mut self, owner: ComponentId, object: T) -> Handle<T> {
        let handle = if let Some(slot_index) = self.slots.iter().position(|slot| slot.is_vacant()) {
            let slot_id = u32::try_from(slot_index).expect("resource slot index exhausted");
            let slot = &mut self.slots[slot_index];
            let generation = slot.generation();
            slot.activate(owner, object);
            Handle::new(slot_id, generation)
        } else {
            let slot_id = u32::try_from(self.slots.len()).expect("resource slot index exhausted");
            let generation = 0;
            self.slots.push(Slot::new(generation, owner, object));
            Handle::new(slot_id, generation)
        };
        emit(TraceEvent::ResourceGrant {
            component: owner,
            kind: self.kind,
            handle: RawHandle::from_raw(handle.to_raw()),
        });
        handle
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得资源对象。
    pub(crate) fn get(&self, caller: ComponentId, handle: Handle<T>) -> Result<&T, HandleError> {
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

    /// `get` 的可变版本：验证规则完全相同（slot/generation/owner/生命周期）。
    /// 供资源表在授权成立后更新自己的 record（如 IRQ 的投递目标注册）。
    pub(crate) fn get_mut(
        &mut self,
        caller: ComponentId,
        handle: Handle<T>,
    ) -> Result<&mut T, HandleError> {
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
        slot.object_mut().ok_or(HandleError::Revoked)
    }

    /// 只读遍历 slot（供各资源表的专属检查使用，如 MMIO 的设备独占）。
    pub(crate) fn slots(&self) -> &[Slot<T>] {
        &self.slots
    }

    /// 可变遍历 slot（供 Core 内部按非 handle 锚点更新的操作使用，如 IRQ 顶半部
    /// 按中断号累计 Polled 事件）。
    pub(crate) fn slots_mut(&mut self) -> &mut [Slot<T>] {
        &mut self.slots
    }

    /// 撤销指定组件拥有的全部 authority（每个被撤销的 authority 各记一笔）。
    pub(crate) fn revoke_owner(&mut self, owner: ComponentId) {
        for index in 0..self.slots.len() {
            let slot = &mut self.slots[index];
            if slot.owner() != owner || slot.is_vacant() {
                continue;
            }
            // revoke() 会 bump generation，所以先算好"撤销前"那个 handle ——
            // 那才是持有者手里此刻失效的那个。
            let handle = Handle::<T>::new(
                u32::try_from(index).expect("resource slot index exhausted"),
                slot.generation(),
            );
            slot.revoke();
            emit(TraceEvent::ResourceRevoke {
                component: owner,
                kind: self.kind,
                handle: RawHandle::from_raw(handle.to_raw()),
            });
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
        emit(TraceEvent::ResourceRevoke {
            component: caller,
            kind: self.kind,
            handle: RawHandle::from_raw(handle.to_raw()),
        });
        Ok(())
    }
}
