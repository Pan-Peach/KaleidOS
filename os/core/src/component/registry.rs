//! 组件注册表：已加载组件的真相 + 生命周期状态机。
//!
//! 现行状态机（实现即契约，见 docs/component-model.md §5 全量生命周期）：
//!
//! ```text
//! Declared --resolve--> Resolved --begin_start--> Starting --finish_start--> Ready
//!     any state --mark_failed--> Failed   （恢复 = 全新实例）
//!     any state --unload--> 记录移除       （phase 1 不回收放段内存）
//! ```
//!
//! `Resolved` = 所有 required Interfaces 都已找到 provider。`Starting` =
//! 正在执行 `kcomp_init()`（此期间 `kcore_interface_publish` 只记录 pending，
//! 不修改 active binding）。`finish_start` 由 Core 在 `kcomp_init()` 返回 0 且
//! pending interfaces 原子提交后调用（见 `component/interface.rs`）。非法转换
//! 返回 Err（Core 验证后才提交状态，Policy proposes 原则）。
//! id 单调递增、不回收：组件实例 = 身份——unload 后重载同组件是新实例（新 id），
//! 失败恢复=全新实例（component-model.md）。

use alloc::vec::Vec;

use crate::component::{ComponentId, ComponentState};
use crate::memory::MemoryLease;
use spin::{Mutex, Once};

const MAX_NAME_LEN: usize = 64;

/// 一个已加载组件（加载完 loader 的调用方填充）。
#[derive(Debug, PartialEq)]
pub struct ComponentRecord {
    pub id: ComponentId,
    pub name: Vec<u8>,
    pub state: ComponentState,
    pub entry: usize,
    pub base: usize,
    #[allow(dead_code)]
    pub(crate) memory: Option<MemoryLease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// 同名组件已存在（组件名唯一）。
    AlreadyDeclared,
    /// 组件 id 不存在（未声明或已卸载）。
    NotFound,
    /// 状态机非法转换（如 Ready 再 start）。
    InvalidTransition,
    /// 名字超长（> MAX_NAME_LEN）。
    NameTooLong,
    /// id 空间耗尽（单调递增）。
    IdExhausted,
}

/// 组件注册表（Core 保留的组件真相）。可构造（测试友好），生产用全局 `init`。
pub struct Registry {
    records: Vec<ComponentRecord>,
    next_id: u64,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            next_id: 1,
        }
    }

    /// 声明一个组件（loader 放段完成 → Declared）。
    pub(crate) fn declare(
        &mut self,
        name: &[u8],
        entry: usize,
        base: usize,
        memory: Option<MemoryLease>,
    ) -> Result<ComponentId, RegistryError> {
        if name.len() > MAX_NAME_LEN {
            return Err(RegistryError::NameTooLong);
        }
        if self.records.iter().any(|r| r.name.as_slice() == name) {
            return Err(RegistryError::AlreadyDeclared);
        }
        let id = ComponentId::from_raw(
            u32::try_from(self.next_id).map_err(|_| RegistryError::IdExhausted)?,
        );
        self.next_id += 1;
        self.records.push(ComponentRecord {
            id,
            name: name.to_vec(),
            state: ComponentState::Declared,
            entry,
            base,
            memory,
        });
        Ok(id)
    }

    /// Declared → Resolved：所有 required Interfaces 已成功绑定。
    /// 无 requires 的组件同样经过此步（vacuous truth：零依赖 = 已满足）。
    pub fn resolve(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        let rec = self.record_mut(id)?;
        if rec.state != ComponentState::Declared {
            return Err(RegistryError::InvalidTransition);
        }
        rec.state = ComponentState::Resolved;
        Ok(())
    }

    /// Resolved → Starting：开始执行 `kcomp_init()`。
    /// `Starting` 期间组件可以 publish 接口（记录为 pending）与创建任务；
    /// 只有 `finish_start`（或失败路径）能离开该状态。
    pub fn begin_start(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        let rec = self.record_mut(id)?;
        if rec.state != ComponentState::Resolved {
            return Err(RegistryError::InvalidTransition);
        }
        rec.state = ComponentState::Starting;
        Ok(())
    }

    /// Starting → Ready：`kcomp_init()` 返回 0 且 pending interfaces 已提交，
    /// 组件可以对外提供 Interface。
    pub fn finish_start(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        let rec = self.record_mut(id)?;
        if rec.state != ComponentState::Starting {
            return Err(RegistryError::InvalidTransition);
        }
        rec.state = ComponentState::Ready;
        Ok(())
    }

    /// 任意状态 → Failed（组件运行失败，Core 标记；恢复 = 全新实例）。
    pub fn mark_failed(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        let rec = self.record_mut(id)?;
        rec.state = ComponentState::Failed;
        Ok(())
    }

    /// 卸载：移除记录（第一版不回收放段内存）。
    pub fn unload(&mut self, id: ComponentId) -> Result<ComponentRecord, RegistryError> {
        let pos = self
            .records
            .iter()
            .position(|r| r.id == id)
            .ok_or(RegistryError::NotFound)?;
        Ok(self.records.remove(pos))
    }

    pub fn get(&self, id: ComponentId) -> Option<&ComponentRecord> {
        self.records.iter().find(|r| r.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ComponentRecord> {
        self.records.iter()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn record_mut(&mut self, id: ComponentId) -> Result<&mut ComponentRecord, RegistryError> {
        self.records
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or(RegistryError::NotFound)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

// —— 全局（boot/core::init 初始化；monitor 等使用全局，测试用 Registry::new()）——

static REGISTRY: Once<Mutex<Registry>> = Once::new();

/// 初始化全局注册表（core::init 调用一次）。
pub fn init() {
    REGISTRY.call_once(|| Mutex::new(Registry::new()));
}

/// 取全局注册表（init 后可用）。
pub fn get_registry() -> &'static Mutex<Registry> {
    REGISTRY.get().expect("registry not initialized")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> Registry {
        Registry::new()
    }

    #[test]
    fn declare_assigns_increasing_ids() {
        let mut reg = r();
        let a = reg.declare(b"a", 0x100, 0x200, None).unwrap();
        let b = reg.declare(b"b", 0x300, 0x400, None).unwrap();
        assert_eq!(a.raw(), 1);
        assert_eq!(b.raw(), 2);
    }

    #[test]
    fn duplicate_name_is_rejected() {
        let mut reg = r();
        reg.declare(b"dup", 1, 2, None).unwrap();
        assert_eq!(
            reg.declare(b"dup", 3, 4, None),
            Err(RegistryError::AlreadyDeclared)
        );
    }

    #[test]
    fn resolve_transitions_declared_to_resolved() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Resolved);
    }

    #[test]
    fn begin_start_transitions_resolved_to_starting() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Starting);
    }

    #[test]
    fn finish_start_transitions_starting_to_ready() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Ready);
    }

    #[test]
    fn finish_start_without_begin_start_is_invalid() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        assert_eq!(reg.finish_start(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Resolved);
    }

    #[test]
    fn begin_start_from_declared_without_resolve_is_invalid() {
        // Declared --begin_start--> Starting 的硬编码已被拆开：必须先 resolve。
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        assert_eq!(reg.begin_start(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Declared);
    }

    #[test]
    fn resolve_twice_is_invalid_transition() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        assert_eq!(reg.resolve(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Resolved);
    }

    #[test]
    fn begin_start_twice_is_invalid_transition() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        assert_eq!(reg.begin_start(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Starting);
    }

    #[test]
    fn begin_start_undeclared_is_not_found() {
        let mut reg = r();
        let ghost = ComponentId::from_raw(99);
        assert_eq!(reg.begin_start(ghost), Err(RegistryError::NotFound));
    }

    #[test]
    fn resolve_undeclared_is_not_found() {
        let mut reg = r();
        let ghost = ComponentId::from_raw(99);
        assert_eq!(reg.resolve(ghost), Err(RegistryError::NotFound));
    }

    #[test]
    fn unload_removes_record() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.unload(id).unwrap();
        assert_eq!(reg.len(), 0);
        assert!(reg.get(id).is_none());
    }

    #[test]
    fn failed_is_reachable_from_any_state() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.mark_failed(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Failed);
    }

    #[test]
    fn name_too_long_is_rejected() {
        let mut reg = r();
        let long = [b'x'; MAX_NAME_LEN + 1];
        assert_eq!(
            reg.declare(&long, 1, 2, None),
            Err(RegistryError::NameTooLong)
        );
    }

    #[test]
    fn name_at_max_length_is_accepted() {
        let mut reg = r();
        let name = [b'x'; MAX_NAME_LEN];
        assert!(reg.declare(&name, 1, 2, None).is_ok());
    }

    #[test]
    fn duplicate_declaration_keeps_original_record() {
        let mut reg = r();
        let id = reg.declare(b"dup", 0x100, 0x200, None).unwrap();
        assert_eq!(
            reg.declare(b"dup", 0x300, 0x400, None),
            Err(RegistryError::AlreadyDeclared)
        );
        let rec = reg.get(id).unwrap();
        assert_eq!(rec.entry, 0x100, "原记录不能被覆盖");
        assert_eq!(rec.state, ComponentState::Declared);
    }

    #[test]
    fn unload_unknown_is_not_found() {
        let mut reg = r();
        assert_eq!(
            reg.unload(ComponentId::from_raw(99)),
            Err(RegistryError::NotFound)
        );
    }

    #[test]
    fn mark_failed_unknown_is_not_found() {
        let mut reg = r();
        assert_eq!(
            reg.mark_failed(ComponentId::from_raw(99)),
            Err(RegistryError::NotFound)
        );
    }

    #[test]
    fn failed_transition_keeps_original_state() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        // Ready 再 begin_start：拒绝，且状态保持 Ready
        assert_eq!(reg.begin_start(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Ready);
    }

    #[test]
    fn begin_start_after_failed_is_invalid_and_keeps_failed() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.mark_failed(id).unwrap();
        assert_eq!(reg.begin_start(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Failed);
    }

    #[test]
    fn unload_returns_record_and_ids_are_not_reused() {
        let mut reg = r();
        let first = reg.declare(b"a", 1, 2, None).unwrap();
        let removed = reg.unload(first).unwrap();
        assert_eq!(removed.id, first);
        assert_eq!(reg.len(), 0);
        // 卸载后重载同名组件 = 新实例（新 id），旧 id 不再复用
        let second = reg.declare(b"a", 3, 4, None).unwrap();
        assert_ne!(second, first);
        assert_eq!(second.raw(), first.raw() + 1);
    }
}
