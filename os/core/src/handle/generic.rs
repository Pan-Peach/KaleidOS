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

    /// 跨 ABI 边界的 opaque 编码：高 32 位 slot、低 32 位 generation。
    ///
    /// 组件侧只能把它当作不透明值传递（`u64`）；token 可被伪造，authority
    /// 依然由资源表的 slot/generation/owner 验证决定。
    pub(crate) const fn to_raw(self) -> u64 {
        ((self.slot as u64) << 32) | self.generation as u64
    }

    /// 从 ABI raw 值重建 handle（不做验证；验证在资源表 `get`/`release`）。
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self::new((raw >> 32) as u32, raw as u32)
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

    pub(crate) const fn generation(&self) -> u32 {
        self.generation
    }

    pub(crate) const fn owner(&self) -> ComponentId {
        self.owner
    }

    pub(crate) fn object(&self) -> Option<&T> {
        self.object.as_ref()
    }

    pub(crate) fn object_mut(&mut self) -> Option<&mut T> {
        self.object.as_mut()
    }

    pub(crate) fn is_vacant(&self) -> bool {
        self.object.is_none()
    }

    /// 在保留 slot index 和 generation 的前提下重新放入资源。
    pub(crate) fn activate(&mut self, owner: ComponentId, object: T) {
        debug_assert!(self.object.is_none());
        self.owner = owner;
        self.object = Some(object);
    }

    /// 撤销当前 authority；重复 revoke 不应再次改变 generation。
    pub(crate) fn revoke(&mut self) {
        if self.object.take().is_some() {
            self.generation = self.generation.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Handle;

    #[test]
    fn raw_roundtrip_preserves_slot_and_generation() {
        let handle = Handle::<u8>::new(3, 7);
        assert_eq!(handle.to_raw(), (3u64 << 32) | 7);
        let decoded = Handle::<u8>::from_raw(handle.to_raw());
        assert_eq!(decoded.slot(), 3);
        assert_eq!(decoded.generation(), 7);
    }

    #[test]
    fn raw_roundtrip_at_u32_limits() {
        let handle = Handle::<u8>::new(u32::MAX, u32::MAX);
        assert_eq!(handle.to_raw(), u64::MAX);
        let decoded = Handle::<u8>::from_raw(u64::MAX);
        assert_eq!(decoded.slot(), u32::MAX);
        assert_eq!(decoded.generation(), u32::MAX);
    }
}
