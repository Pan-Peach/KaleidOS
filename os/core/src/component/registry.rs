//! 组件注册表：已加载组件的真相 + 生命周期状态机。
//!
//! 状态机：Declared → Starting → Ready；任何阶段可进入 Failed。
//! 非法转换返回 Err（Core 验证后才提交状态，Policy proposes 原则）。
//! id 单调递增、不回收：组件实例 = 身份——unload 后重载同组件是新实例（新 id），
//! 失败恢复=全新实例（component-model.md）。第一版 unload 不释放放段内存
//! （phase 1 不承诺组件内存回收，见 AGENTS.md）。

use alloc::vec::Vec;

use crate::component::{ComponentId, ComponentState};
use crate::memory::MemoryLease;
use spin::{Mutex, Once};

const MAX_NAME_LEN: usize = 64;

/// 一个已加载组件（加载完 loader 的调用方填充）。
#[derive(Debug)]
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

    /// Declared → Ready（加载链完成，组件可以提供服务）。
    pub fn start(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        let rec = self.record_mut(id)?;
        if rec.state != ComponentState::Declared {
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
    fn start_transitions_declared_to_ready() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.start(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Ready);
    }

    #[test]
    fn start_twice_is_invalid_transition() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.start(id).unwrap();
        assert_eq!(reg.start(id), Err(RegistryError::InvalidTransition));
    }

    #[test]
    fn start_undeclared_is_not_found() {
        let mut reg = r();
        let ghost = ComponentId::from_raw(99);
        assert_eq!(reg.start(ghost), Err(RegistryError::NotFound));
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
        reg.start(id).unwrap();
        reg.mark_failed(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Failed);
    }

    #[test]
    fn name_too_long_is_rejected() {
        let mut reg = r();
        let long = [b'x'; MAX_NAME_LEN + 1];
        assert_eq!(reg.declare(&long, 1, 2, None), Err(RegistryError::NameTooLong));
    }
}
