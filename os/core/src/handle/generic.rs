//! 类型化 Handle 与资源 slot 的共同形状。

// 当前资源表仍是骨架，字段和 accessor 会在 C6 的 grant/get/revoke 实现中
// 被使用；先保留完整形状，避免为了消除暂时的 dead_code 警告删掉 authority 真相。
#![allow(dead_code)]

use crate::component::ComponentId;
use core::marker::PhantomData;

/// 类型化的 Core authority token。
///
/// `slot`/`generation` 可以被组件猜测或伪造；真正的 authority 由资源表在
/// Core 内验证。这种 token 只保证不同资源类型不能在 Rust API 中混用。
pub struct Handle<T> {
    slot: u32,
    generation: u32,
    _marker: PhantomData<T>,
}

impl<T> Handle<T> {
    /// 仅由 Core 资源表创建 Handle。
    pub(crate) const fn new(slot: u32, generation: u32) -> Self {
        Self {
            slot,
            generation,
            _marker: PhantomData,
        }
    }

    pub(crate) const fn slot(self) -> u32 {
        self.slot
    }

    pub(crate) const fn generation(self) -> u32 {
        self.generation
    }
}

impl<T> Copy for Handle<T> {}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> core::fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Handle")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .finish()
    }
}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.slot == other.slot && self.generation == other.generation
    }
}

impl<T> Eq for Handle<T> {}

/// 资源表中的 Core truth：generation、owner 和可被 revoke 的 object。
pub(crate) struct Slot<T> {
    pub(crate) generation: u32,
    pub(crate) owner: ComponentId,
    pub(crate) object: Option<T>,
}

impl<T> Slot<T> {
    pub(crate) const fn new(generation: u32, owner: ComponentId, object: T) -> Self {
        Self {
            generation,
            owner,
            object: Some(object),
        }
    }
}
