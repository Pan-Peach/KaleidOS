//! 组件注册表：已加载组件的真相 + 生命周期状态机。
//!
//! 现行状态机（实现即契约，见 docs/component-model.md §5 全量生命周期）：
//!
//! ```text
//! Declared --resolve--> Resolved --begin_start--> Starting --finish_start--> Ready
//!     Ready --begin_stop--> Stopping --finish_stop--> Stopped   （stop 路径仅声明，未接线）
//!     any state --mark_failed--> Failed   （恢复 = 全新实例）
//!     any state --unload--> 记录移除       （phase 1 不回收放段内存）
//! ```
//!
//! **合法转换的唯一真相是 [`ComponentState::can_transition`]**（Core owns truth）：
//! 本模块所有转换方法都经私有 `transition` 收口，只做存在性检查 + 规则校验 + 提交，
//! 不再各自硬编码 `state != X`。非法转换返回 Err（Core 验证后才提交状态，
//! Policy proposes 原则）。
//!
//! `Resolved` = 所有 required Interfaces 都已找到 provider。`Starting` =
//! 正在执行 `kcomp_init()`（此期间 `kcore_interface_publish` 只记录 pending，
//! 不修改 active binding）。`finish_start` 由 Core 在 `kcomp_init()` 返回 0 且
//! pending interfaces 原子提交后调用（见 `component/interface.rs`）。
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
    /// 可选退出入口（`kcomp_exit`，Linux `module_exit` 类比）地址；仅当组件
    /// 导出该符号时存在。
    ///
    /// **本轮是 seam only：Core 记录但从不调用**——与 `handle/generic.rs`
    /// 保留未使用形状的先例一致。
    ///
    /// TODO(component-exit): 未来 ComponentManager 的 stop 路径会调用它并推进
    /// `Stopping → Stopped`；当前没有任何路径读它。
    #[allow(dead_code)]
    pub exit: Option<usize>,
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
            exit: None,
            memory,
        });
        // 出生也入 trace：否则"只声明未 resolve"的实例在事件流里不可见，
        // 而"失败组件是否被回收"这类断言需要看到它从哪来。
        crate::trace::emit(crate::trace::TraceEvent::ComponentState {
            component: id,
            from: None,
            to: ComponentState::Declared,
        });
        Ok(id)
    }

    /// 记录组件的可选退出入口（`kcomp_exit`，Linux `module_exit` 类比）。
    ///
    /// 与 `declare` 分开：只有 load 路径有真实值，registry 单测只关心状态机，
    /// 不必让每个调用点都携带 `exit`。**Core 本轮只存不调用**（seam only）。
    pub fn record_exit(
        &mut self,
        id: ComponentId,
        exit: Option<usize>,
    ) -> Result<(), RegistryError> {
        self.record_mut(id)?.exit = exit;
        Ok(())
    }

    /// Declared → Resolved：所有 required Interfaces 已成功绑定。
    /// 无 requires 的组件同样经过此步（vacuous truth：零依赖 = 已满足）。
    pub fn resolve(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Resolved)
    }

    /// Resolved → Starting：开始执行 `kcomp_init()`。
    /// `Starting` 期间组件可以 publish 接口（记录为 pending）与创建任务；
    /// 只有 `finish_start`（或失败路径）能离开该状态。
    pub fn begin_start(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Starting)
    }

    /// Starting → Ready：`kcomp_init()` 返回 0 且 pending interfaces 已提交，
    /// 组件可以对外提供 Interface。
    pub fn finish_start(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Ready)
    }

    /// Ready → Stopping：开始优雅停止。
    ///
    /// **仅声明转换，无生产调用方。** 合法边由 [`ComponentState::can_transition`]
    /// 唯一定义；stop orchestration（调用 `kcomp_exit`、quiesce/drain、authority
    /// 回收）尚未接线。
    ///
    /// TODO(component-exit): 未来 ComponentManager 的 stop 路径驱动本方法，
    /// 随后 `finish_stop`；当前停在声明层——编排 seam 骨架见
    /// `component/exit.rs::stop_component`（同样尚未接线）。
    pub fn begin_stop(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Stopping)
    }

    /// Stopping → Stopped：停止完成。
    ///
    /// **仅声明转换，无生产调用方**（见 [`Self::begin_stop`] 的
    /// `TODO(component-exit)`）。
    ///
    /// TODO(component-exit): 未来 stop 路径在 `kcomp_exit` 返回、authority 回收后
    /// 调用本方法；当前停在声明层。
    pub fn finish_stop(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Stopped)
    }

    /// 任意状态 → Failed（组件运行失败，Core 标记；恢复 = 全新实例）。
    pub fn mark_failed(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Failed)
    }

    /// 生命周期转换的**唯一收口**：存在性检查（NotFound）→ 规则校验
    /// （[`ComponentState::can_transition`]）→ 提交。所有转换方法都经此，
    /// 合法转换表只存在于一处。
    fn transition(&mut self, id: ComponentId, to: ComponentState) -> Result<(), RegistryError> {
        let rec = self.record_mut(id)?;
        let from = rec.state;
        if !from.can_transition(to) {
            return Err(RegistryError::InvalidTransition);
        }
        rec.state = to;
        // 所有合法转换都在这里落一笔 —— 单点覆盖，任何新转换入口自动被记录。
        crate::trace::emit(crate::trace::TraceEvent::ComponentState {
            component: id,
            from: Some(from),
            to,
        });
        Ok(())
    }

    /// `id` 是否为 `Failed`（逻辑死亡）实例。
    ///
    /// Core 真相门禁：失败实例不得获取新 authority 或创建新 work，但仍需
    /// teardown（`release` / `revoke`、释放已持有的 handle）。（`Stopping` /
    /// `Stopped` 落地后并入本判定：它们同样不是可运行状态。）
    pub fn is_failed(&self, id: ComponentId) -> bool {
        self.get(id)
            .is_some_and(|r| r.state == ComponentState::Failed)
    }

    /// 该组件拥有的任务是否允许运行。
    ///
    /// 只有活着的实例（`Starting` = `kcomp_init` 执行期、`Ready`）可以运行任务；
    /// `Failed`、`Stopping`、`Stopped` 实例的任务必须从 runnable 候选中剔除，并在
    /// commit 前再次验证（`Stopping` / `Stopped` 的转换已由规则表声明，但当前没有
    /// 生产路径驱动它们——stop orchestration deferred；门禁语义已经正确）。
    pub fn may_run(&self, id: ComponentId) -> bool {
        self.get(id)
            .is_some_and(|r| matches!(r.state, ComponentState::Starting | ComponentState::Ready))
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

    /// 驱一个组件走完 init 路径到 `Ready`（stop 路径唯一合法的起点）。
    fn ready(reg: &mut Registry) -> ComponentId {
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    #[test]
    fn declare_assigns_increasing_ids() {
        let mut reg = r();
        let a = reg.declare(b"a", 0x100, 0x200, None).unwrap();
        let b = reg.declare(b"b", 0x300, 0x400, None).unwrap();
        assert_eq!(a.raw(), 1);
        assert_eq!(b.raw(), 2);
    }

    /// 对抗：非法转换被拒绝时，**真相不变**。
    ///
    /// 不断言"没有产生事件"：trace 只记录合法提交（`emit` 位于状态提交之后），
    /// 非法转换本就永远不会出现在事件流里，"事件为空"是不可证伪的断言。
    /// 真正有意义的是：被拒绝的尝试改不动真相。
    #[test]
    fn rejected_transition_leaves_truth_unchanged() {
        init();
        let id = get_registry()
            .lock()
            .declare(b"registry_rejected_transition", 1, 2, None)
            .unwrap();

        // Declared → Ready 非法（跳过 Resolved / Starting）。
        assert_eq!(
            get_registry().lock().finish_start(id),
            Err(RegistryError::InvalidTransition)
        );
        assert_eq!(
            get_registry().lock().get(id).unwrap().state,
            crate::component::ComponentState::Declared,
            "被拒绝的转换不得改变真相"
        );

        // 其它非法边（Declared → Starting）同样不动真相。
        assert_eq!(
            get_registry().lock().begin_start(id),
            Err(RegistryError::InvalidTransition)
        );
        assert_eq!(
            get_registry().lock().get(id).unwrap().state,
            crate::component::ComponentState::Declared
        );
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
    fn is_failed_only_reports_failed_instances() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        assert!(!reg.is_failed(id));
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        assert!(!reg.is_failed(id));
        reg.finish_start(id).unwrap();
        assert!(!reg.is_failed(id));
        reg.mark_failed(id).unwrap();
        assert!(reg.is_failed(id));
        assert!(
            !reg.is_failed(ComponentId::from_raw(99)),
            "unknown is not Failed"
        );
    }

    #[test]
    fn may_run_only_for_live_instances() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        assert!(!reg.may_run(id), "Declared does not run work");
        reg.resolve(id).unwrap();
        assert!(!reg.may_run(id), "Resolved does not run work");
        reg.begin_start(id).unwrap();
        assert!(reg.may_run(id), "Starting (kcomp_init) may create/run work");
        reg.finish_start(id).unwrap();
        assert!(reg.may_run(id), "Ready runs work");
        reg.mark_failed(id).unwrap();
        assert!(!reg.may_run(id), "Failed must not run work");
        assert!(
            !reg.may_run(ComponentId::from_raw(99)),
            "unknown must not run"
        );
    }

    #[test]
    fn may_run_excludes_stopping_stopped_and_failed() {
        // Given：一个 Ready 组件（唯一可运行任务的活状态）。
        let mut reg = r();
        let id = ready(&mut reg);
        assert!(reg.may_run(id), "Ready runs work");

        // When / Then：stop 路径上的状态（经规则表驱动）同样不得运行任务。
        reg.begin_stop(id).unwrap();
        assert!(!reg.may_run(id), "Stopping must not run work");
        reg.finish_stop(id).unwrap();
        assert!(!reg.may_run(id), "Stopped must not run work");
        reg.mark_failed(id).unwrap();
        assert!(!reg.may_run(id), "Failed must not run work");
    }

    #[test]
    fn begin_stop_and_finish_stop_drive_ready_to_stopped() {
        // Given：一个 Ready 组件。
        let mut reg = r();
        let id = ready(&mut reg);

        // When / Then：Ready → Stopping → Stopped 由规则表放行。
        reg.begin_stop(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Stopping);
        reg.finish_stop(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Stopped);
    }

    #[test]
    fn begin_stop_is_only_legal_from_ready() {
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();
        assert_eq!(reg.begin_stop(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Declared);
        reg.resolve(id).unwrap();
        assert_eq!(reg.begin_stop(id), Err(RegistryError::InvalidTransition));
        reg.begin_start(id).unwrap();
        assert_eq!(reg.begin_stop(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Starting);
    }

    #[test]
    fn finish_stop_without_begin_stop_is_invalid() {
        let mut reg = r();
        let id = ready(&mut reg);
        assert_eq!(reg.finish_stop(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Ready);
    }

    #[test]
    fn stopping_may_transition_to_failed() {
        let mut reg = r();
        let id = ready(&mut reg);
        reg.begin_stop(id).unwrap();
        reg.mark_failed(id).unwrap();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Failed);
    }

    #[test]
    fn stopped_cannot_restart() {
        let mut reg = r();
        let id = ready(&mut reg);
        reg.begin_stop(id).unwrap();
        reg.finish_stop(id).unwrap();
        assert_eq!(reg.begin_start(id), Err(RegistryError::InvalidTransition));
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Stopped);
    }

    #[test]
    fn record_exit_stores_seam_and_declare_defaults_to_none() {
        // Given：一个新声明的组件。
        let mut reg = r();
        let id = reg.declare(b"x", 1, 2, None).unwrap();

        // Then：exit seam 默认 None（组件不导出 kcomp_exit）。
        assert_eq!(reg.get(id).unwrap().exit, None);

        // When：loader 记录一个可选退出入口。
        reg.record_exit(id, Some(0x3000)).unwrap();

        // Then：记录可见；未知 id 被拒绝。
        assert_eq!(reg.get(id).unwrap().exit, Some(0x3000));
        assert_eq!(
            reg.record_exit(ComponentId::from_raw(99), Some(1)),
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

    /// 性能基线（`make bench`）：**component 数量增长时的 lookup 趋势**。
    ///
    /// 用局部 `Registry`（`r()`），不碰全局真相，因此不需要串行锁。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_component_scaling() {
        crate::bench::report_environment();
        const SIZES: [(usize, &str); 3] = [
            (1, "component.lookup.n1"),
            (32, "component.lookup.n32"),
            (256, "component.lookup.n256"),
        ];
        for (count, name) in SIZES {
            let mut reg = r();
            let mut ids = alloc::vec::Vec::new();
            for index in 0..count {
                let label = alloc::format!("comp{index}");
                ids.push(reg.declare(label.as_bytes(), 1, 2, None).unwrap());
            }
            let probe = ids[count / 2];
            let mut bench = crate::bench::Bench::new(name);
            bench.run(100, || reg.get(probe).is_some());
            bench.finish().report();
        }
    }
}
