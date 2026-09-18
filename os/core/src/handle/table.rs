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

#[cfg(test)]
mod tests {
    use super::{Handle, HandleError, ResourceKind, ResourceTable};
    use crate::component::ComponentId;
    use alloc::vec::Vec;
    use proptest::prelude::*;

    /// 固定的小 owner 域（3 个），让 shrink 时跨 owner 的 `WrongOwner` 分支可复现。
    const OWNER_COUNT: usize = 3;
    const OWNERS: [ComponentId; OWNER_COUNT] = [
        ComponentId::from_raw(1),
        ComponentId::from_raw(2),
        ComponentId::from_raw(3),
    ];

    /// 参考模型的单个 slot：与 Core 真相一一对应（generation / owner / object）。
    #[derive(Debug, Clone, Copy)]
    struct ModelSlot {
        generation: u32,
        owner: ComponentId,
        object: Option<u32>,
    }

    /// 参考模型：每个 slot 的真相 + 本次运行中所有 grant 真实返回过的 handle。
    struct Model {
        slots: Vec<ModelSlot>,
        history: Vec<Handle<u32>>,
        /// 同时存活资源数的历史峰值（= slot 数组应停住的高水位）。
        peak_live: usize,
    }

    impl Model {
        fn new() -> Self {
            Self {
                slots: Vec::new(),
                history: Vec::new(),
                peak_live: 0,
            }
        }

        /// 复刻 `Slot::revoke`：只有仍持有 object 时才 bump generation。
        fn revoke_slot(&mut self, index: usize) {
            if self.slots[index].object.take().is_some() {
                self.slots[index].generation = self.slots[index].generation.wrapping_add(1);
            }
        }
    }

    /// 一次 get/get_mut/release 使用的 handle 来源。
    ///
    /// `History` 只会挑 Core 真实返回过的 handle（不手造）；`OutOfRange` 是唯一
    /// 允许的手造 handle，用来钉住 `Invalid` 分支。
    #[derive(Debug, Clone, Copy)]
    enum HandlePick {
        History(usize),
        OutOfRange(u32),
    }

    /// 对 `ResourceTable<u32>` 的一个操作（每个变体是一条 strategy arm）。
    #[derive(Debug, Clone, Copy)]
    enum Op {
        Grant { owner: usize, value: u32 },
        Get { pick: HandlePick, caller: usize },
        GetMut { pick: HandlePick, caller: usize },
        Release { pick: HandlePick, caller: usize },
        RevokeOwner { owner: usize },
    }

    fn owner_strategy() -> impl Strategy<Value = usize> {
        0usize..OWNER_COUNT
    }

    fn value_strategy() -> impl Strategy<Value = u32> {
        0u32..4
    }

    fn pick_strategy() -> impl Strategy<Value = HandlePick> {
        prop_oneof![
            (0usize..64).prop_map(HandlePick::History),
            any::<u32>().prop_map(HandlePick::OutOfRange),
        ]
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (owner_strategy(), value_strategy())
                .prop_map(|(owner, value)| Op::Grant { owner, value }),
            (pick_strategy(), owner_strategy()).prop_map(|(pick, caller)| Op::Get { pick, caller }),
            (pick_strategy(), owner_strategy())
                .prop_map(|(pick, caller)| Op::GetMut { pick, caller }),
            (pick_strategy(), owner_strategy())
                .prop_map(|(pick, caller)| Op::Release { pick, caller }),
            owner_strategy().prop_map(|owner| Op::RevokeOwner { owner }),
        ]
    }

    /// 有界随机序列：1..=60 个 op，覆盖 grant 与全部校验路径。
    fn op_seq() -> impl Strategy<Value = Vec<Op>> {
        proptest::collection::vec(op_strategy(), 1..=60)
    }

    /// 复刻 `ResourceTable::get` 的判定顺序：
    /// slot 越界 → `Invalid`；generation 不符 → `Stale`；owner 不符 → `WrongOwner`；
    /// slot 已空 → `Revoked`。（`get_mut` 规则相同。）
    fn predict_get(
        model: &Model,
        caller: ComponentId,
        handle: Handle<u32>,
    ) -> Result<u32, HandleError> {
        let slot = model
            .slots
            .get(handle.slot() as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != handle.generation() {
            return Err(HandleError::Stale);
        }
        if slot.owner != caller {
            return Err(HandleError::WrongOwner);
        }
        slot.object.ok_or(HandleError::Revoked)
    }

    /// 复刻 `ResourceTable::release` 的判定顺序（无 `Revoked` 分支：
    /// 只要 slot/generation/owner 成立就 Ok）。
    fn predict_release(
        model: &Model,
        caller: ComponentId,
        handle: Handle<u32>,
    ) -> Result<(), HandleError> {
        let slot = model
            .slots
            .get(handle.slot() as usize)
            .ok_or(HandleError::Invalid)?;
        if slot.generation != handle.generation() {
            return Err(HandleError::Stale);
        }
        if slot.owner != caller {
            return Err(HandleError::WrongOwner);
        }
        Ok(())
    }

    fn resolve(pick: HandlePick, model: &Model) -> Option<Handle<u32>> {
        match pick {
            HandlePick::History(index) => {
                // 尚无 grant：没有真实 handle 可用，跳过该 op（OutOfRange 仍可跑）。
                if model.history.is_empty() {
                    None
                } else {
                    Some(model.history[index % model.history.len()])
                }
            }
            HandlePick::OutOfRange(generation) => Some(Handle::new(u32::MAX, generation)),
        }
    }

    /// 每个 op 之后都跑一遍：真实表必须与模型逐 slot 一致，且所有历史 handle
    /// 在所有 caller 下的解析结果都必须被模型精确预测。
    fn check_invariants(table: &mut ResourceTable<u32>, model: &mut Model) {
        // (1)(2)(3)(6) Core truth == 模型 truth：slot/generation/owner/object 全等。
        // 模型只在 revoke/release 时 wrapping_add(1)，相等即蕴含 generation 单调不减。
        assert_eq!(
            table.slots().len(),
            model.slots.len(),
            "slot count must match model"
        );
        for (index, (real, expected)) in table.slots().iter().zip(model.slots.iter()).enumerate() {
            assert_eq!(
                real.generation(),
                expected.generation,
                "slot {index}: generation diverged"
            );
            assert_eq!(real.owner(), expected.owner, "slot {index}: owner diverged");
            assert_eq!(
                real.is_vacant(),
                expected.object.is_none(),
                "slot {index}: liveness diverged"
            );
            assert_eq!(
                real.object().copied(),
                expected.object,
                "slot {index}: object diverged"
            );
        }

        // (5) 空 slot 必须被优先复用：slot 数组长度 == 同时存活资源数的高水位
        // （grant 只在所有 slot 都存活时才 +1，之后必被释放的 slot 复用）。
        let live = model
            .slots
            .iter()
            .filter(|slot| slot.object.is_some())
            .count();
        model.peak_live = model.peak_live.max(live);
        assert!(
            live <= table.slots().len(),
            "live resources cannot exceed slots"
        );
        assert_eq!(
            table.slots().len(),
            model.peak_live,
            "slots must stop at the simultaneous-live high-water mark"
        );

        // (2)(3)(6) 每个历史 handle × 每个 owner：get/get_mut 结果必须与模型预测逐位
        // 相等——stale handle 永不解析、跨 owner 必 WrongOwner、revoked owner 的 handle
        // 失效而其他 owner 的存活 handle 照常工作。
        for &handle in &model.history {
            for &caller in &OWNERS {
                let want = predict_get(model, caller, handle);
                assert_eq!(
                    table.get(caller, handle).copied(),
                    want,
                    "get({caller:?}, {handle:?}) diverged"
                );
                assert_eq!(
                    table.get_mut(caller, handle).copied(),
                    want,
                    "get_mut({caller:?}, {handle:?}) diverged"
                );
            }
        }
    }

    fn apply(table: &mut ResourceTable<u32>, model: &mut Model, op: Op) {
        match op {
            Op::Grant { owner, value } => {
                // Given: 该资源尚不存在；When: grant(owner, value)。
                // 预期复用第一个空 slot（保留其 generation），否则在尾部新增 gen=0 的 slot。
                let owner = OWNERS[owner];
                let expected = model.slots.iter().position(|slot| slot.object.is_none());
                let (index, generation) = match expected {
                    Some(index) => (index, model.slots[index].generation),
                    None => (model.slots.len(), 0),
                };
                let handle = table.grant(owner, value);
                // Then: handle 必须指向预测的 slot，且 generation 与模型一致。
                assert_eq!(
                    handle.slot() as usize,
                    index,
                    "grant must reuse the first vacant slot"
                );
                assert_eq!(
                    handle.generation(),
                    generation,
                    "reuse must preserve generation"
                );
                if index == model.slots.len() {
                    model.slots.push(ModelSlot {
                        generation,
                        owner,
                        object: Some(value),
                    });
                } else {
                    model.slots[index].owner = owner;
                    model.slots[index].object = Some(value);
                }
                model.history.push(handle);
            }
            Op::Get { pick, caller } => {
                if let Some(handle) = resolve(pick, model) {
                    let caller = OWNERS[caller];
                    // Then: get 的结果（含 Ok 时的值）必须与模型预测一致。
                    assert_eq!(
                        table.get(caller, handle).copied(),
                        predict_get(model, caller, handle)
                    );
                }
            }
            Op::GetMut { pick, caller } => {
                if let Some(handle) = resolve(pick, model) {
                    let caller = OWNERS[caller];
                    // get_mut 与 get 的验证规则完全相同。
                    assert_eq!(
                        table.get_mut(caller, handle).copied(),
                        predict_get(model, caller, handle)
                    );
                }
            }
            Op::Release { pick, caller } => {
                if let Some(handle) = resolve(pick, model) {
                    let caller = OWNERS[caller];
                    let result = table.release(caller, handle);
                    assert_eq!(result, predict_release(model, caller, handle));
                    // (7) 二次 release 时会因 generation 已 bump 而 Stale；模型只在
                    // release 成功时同步一次 revoke，因此 Stale 判定天然成立。
                    if result.is_ok() {
                        model.revoke_slot(handle.slot() as usize);
                    }
                }
            }
            Op::RevokeOwner { owner } => {
                let owner = OWNERS[owner];
                table.revoke_owner(owner);
                // 仅该 owner 的存活 slot 失效（generation +1）；空 slot 与其他 owner 不动
                // （revoke_slot 对空 slot 是 no-op，与 Core 的 is_vacant 跳过等价）。
                for index in 0..model.slots.len() {
                    if model.slots[index].owner == owner {
                        model.revoke_slot(index);
                    }
                }
            }
        }
        check_invariants(table, model);
    }

    /// 验收：slot 复用必须让**旧 handle 立即 stale，即使复用者是同一个 owner**。
    ///
    /// 这是"为什么 handle 必须带 generation"的具体危害（ABA）：组件 A 释放资源 R
    /// 后用同一 slot grant 新资源 R'，若 handle 只有 slot index，A 手里过期的 R
    /// handle 会在 R' 上重新"命中"，把 authority 指向一个完全不同的对象。
    /// `revoke` 在复用前 bump generation，正是为堵死这条路径。
    ///
    /// review 依据：`docs/resource-model-review.md` §C.2（`AddressSpaceHandle` 的
    /// generation 恒为 1、manager 从不实例化——没有 slot 复用就不需要 generation，
    /// 反证复用必须靠 generation 区分）与 §0 对提案 §3（handle-vs-ID）的裁定：
    /// 移除 handle 不会带来安全收益，typed ID 恰恰保住了 generation/类型/O(1)。
    #[test]
    fn reused_slot_stales_old_handle_for_same_owner() {
        let owner = OWNERS[0];
        let mut table: ResourceTable<u32> = ResourceTable::new(ResourceKind::Mmio);

        let before = table.grant(owner, 0xAA);
        assert_eq!(table.get(owner, before), Ok(&0xAA));

        // 释放后立即用**同一个 owner** 复用同一 slot。
        assert_eq!(table.release(owner, before), Ok(()));
        let after = table.grant(owner, 0xBB);

        // 同一 slot、同一 owner，但 generation 已前进 → 旧 handle 必 Stale。
        assert_eq!(after.slot(), before.slot(), "grant 必须复用空 slot");
        assert_ne!(
            after.generation(),
            before.generation(),
            "slot 复用必须 bump generation，否则旧 handle 会 ABA 命中"
        );
        assert_eq!(table.get(owner, before), Err(HandleError::Stale));
        assert_eq!(table.get(owner, after), Ok(&0xBB));
    }

    proptest! {
        /// 模型对照的随机生命周期：随机 grant/get/get_mut/release/revoke_owner 序列
        /// 之后，真实 `ResourceTable<u32>` 的 slot/generation/owner 生命周期必须与
        /// 参考模型逐操作一致（authority 真相不可撒谎）。
        #[test]
        fn random_handle_lifecycles_keep_authority_invariants(ops in op_seq()) {
            let mut table: ResourceTable<u32> = ResourceTable::new(ResourceKind::Mmio);
            let mut model = Model::new();
            for op in ops {
                apply(&mut table, &mut model, op);
            }
        }
    }
}
