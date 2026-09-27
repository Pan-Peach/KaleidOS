//! 组件注册表：一个 `ComponentId` 对应一个**完整运行组件** + 生命周期状态机。
//!
//! 身份模型：`ComponentId` 是 Core runtime 中唯一的一级运行身份。它同时拥有
//! 自己那份**加载并重定位后的镜像**（`LoadedComponent`：段基址 / 入口 / 镜像
//! 大小 / `kcomp_abi` / `MemoryLease`）——`artifact` 只是被加载的字节，
//! 每次 load/instantiate 都得到一份独立的 writable image state。
//!
//! 现行状态机（实现即契约，见 docs/architecture/component-model.md §5 全量生命周期）：
//!
//! ```text
//! Declared --resolve--> Resolved --begin_start--> Starting --finish_start--> Ready
//!     Ready --begin_stop--> Stopping --finish_stop--> Stopped   （stop 编排：component/exit.rs）
//!     any state --mark_failed--> Failed   （恢复 = 全新组件）
//!     （无 unload：Stopped/Failed 记录留作 tombstone，见契约 §8/§9）
//! ```
//!
//! **合法转换的唯一真相是 [`ComponentState::can_transition`]**（Core owns truth）：
//! 本模块所有转换方法都经私有 `transition` 收口，只做存在性检查 + 规则校验 + 提交，
//! 不再各自硬编码 `state != X`。非法转换返回 Err（Core 验证后才提交状态，
//! Policy proposes 原则）。
//!
//! `Resolved` = 所有 required Endpoints 都已找到 provider。`Starting` =
//! 正在执行 `kcomp_instance_create(args, &out_state)`（此期间 `kcore_endpoint_publish`
//! 只记录 pending，不创建 endpoint）。`finish_start` 由 Core 在 create 返回 0、
//! Core 记录 `instance_state`、且 pending endpoints 原子提交后调用（见
//! `component/load.rs`）。id 单调递增、不回收：组件 = 身份——失败恢复 =
//! 全新组件（新 id），`ComponentId` 永不复用（docs/architecture/component-model.md）。
//!
//! `restart` 语义：从同一个 artifact **重新 instantiate** 一个全新组件——全新
//! `ComponentId`、全新 writable image state（`.data` / `.bss` 回到 artifact 初始
//! 状态）、全新资源 / endpoint。旧组件的 tombstone 记录保留。
//!
//! In-flight call 记账：`ComponentRecord::inflight` 记录该组件尚未返回的调用数
//! （`begin_call` / `finish_call` / `active_calls`）。只有 `Ready` 组件可以
//! `begin_call`；`finish_call` 不设门禁——组件离开 `Ready`（停止 / 失败）时
//! 已在飞行的调用仍须能归还计数。

use alloc::vec::Vec;

use crate::component::endpoint::ExecutionDomain;
use crate::component::loader::LoadedComponent;
use crate::component::{ComponentId, ComponentState};
use crate::memory::address_space::AddressSpaceHandle;
use spin::{Mutex, Once};

/// 一个组件（`declare` 时一次性填充：它拥有自己的 loaded image）。
///
/// - `state` 归组件（Core 状态机唯一真相）；
/// - `loaded` 是**这个组件自己的**加载结果（段放置 / 重定位 / `MemoryLease`），
///   1:1 归属——没有共享镜像表；
/// - `instance_state` 是 `kcomp_instance_create` 写回的 opaque 指针：Core
///   只存/传，不解释、不释放；`NULL` = 无状态组件。
#[derive(Debug, PartialEq)]
pub struct ComponentRecord {
    pub id: ComponentId,
    /// artifact 名（不含 `.kcomp` 后缀）；诊断 / monitor 用。
    pub name: Vec<u8>,
    pub state: ComponentState,
    pub execution_domain: ExecutionDomain,
    pub address_space: Option<AddressSpaceHandle>,
    /// 这一个组件自己的 loaded program（base / create / destroy /
    /// service_dispatch / text_size / abi / MemoryLease）。
    pub loaded: LoadedComponent,
    pub instance_state: *mut (),
    /// 未完成的 consumer→provider 调用计数（`begin_call` / `finish_call`）。
    pub inflight: u32,
}

// `instance_state` 是组件 opaque 指针：Registry 只存取、永不解引用。
// 跨线程使用由外层 `Mutex` 串行化（与 endpoint.rs 的 EndpointRecord 同一理由）。
unsafe impl Send for ComponentRecord {}
unsafe impl Sync for ComponentRecord {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// 实例 id 不存在（未声明；无 unload）。
    NotFound,
    /// 状态机非法转换（如 Ready 再 start）。
    InvalidTransition,
    /// `begin_call`：实例存在但不在 `Ready`（只有 Ready 可开始服务调用）。
    NotReady,
    /// `begin_call`：`inflight` 计数溢出（u32）；拒绝且不改计数。
    CallOverflow,
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

    /// 声明一个组件（`Declared`）。`loaded` 是这个组件自己的加载结果，
    /// 1:1 归它所有；`name` 只是诊断 / monitor 用的 artifact 名。
    ///
    /// 每次 load / instantiate 都是一次 `declare`——同名 artifact 加载两次会得到
    /// **两个独立组件**（各自的 writable image state），不再按名字复用镜像。
    pub(crate) fn declare(
        &mut self,
        name: &[u8],
        loaded: LoadedComponent,
        kind: ExecutionDomain,
    ) -> Result<ComponentId, RegistryError> {
        let id = ComponentId::from_raw(
            u32::try_from(self.next_id).map_err(|_| RegistryError::IdExhausted)?,
        );
        self.next_id += 1;
        self.records.push(ComponentRecord {
            id,
            name: name.to_vec(),
            state: ComponentState::Declared,
            execution_domain: kind,
            address_space: None,
            loaded,
            instance_state: core::ptr::null_mut(),
            inflight: 0,
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

    /// 记录 `kcomp_instance_create` 写回的 opaque state 指针（Core 只存）。
    ///
    /// 生产调用方是 `component/load.rs`：create 返回 0 后、提交 pending
    /// endpoints 之前调用。失败/未完整构造的实例不经此路径（不调用 create）。
    pub fn record_instance_state(
        &mut self,
        id: ComponentId,
        instance_state: *mut (),
    ) -> Result<(), RegistryError> {
        self.record_mut(id)?.instance_state = instance_state;
        Ok(())
    }

    pub fn record_address_space(
        &mut self,
        id: ComponentId,
        address_space: AddressSpaceHandle,
    ) -> Result<(), RegistryError> {
        let record = self.record_mut(id)?;
        if record.execution_domain == ExecutionDomain::KernelNative {
            return Err(RegistryError::InvalidTransition);
        }
        record.address_space = Some(address_space);
        Ok(())
    }

    /// Declared → Resolved：所有 required Endpoints 已成功绑定。
    /// 无 requires 的组件同样经过此步（vacuous truth：零依赖 = 已满足）。
    pub fn resolve(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Resolved)
    }

    /// Resolved → Starting：开始执行 `kcomp_instance_create`。
    /// `Starting` 期间组件可以 publish endpoint（记录为 pending）与创建任务；
    /// 只有 `finish_start`（或失败路径）能离开该状态。
    pub fn begin_start(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Starting)
    }

    /// Starting → Ready：create 返回 0、`instance_state` 已记录且 pending
    /// endpoints 已提交，组件可以对外提供 endpoint。
    pub fn finish_start(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Ready)
    }

    /// Ready → Stopping：开始优雅停止。
    ///
    /// 合法边由 [`ComponentState::can_transition`] 唯一定义；生产调用方是
    /// `component/exit.rs::stop_component`（在确认实例不拥有未退出任务之后）。
    /// 提交后 `may_run` 立即不再放行该实例的任务——"停止中仍等待任务收尾"不在
    /// 本版语义内（drain variant 明确未实现）。
    pub fn begin_stop(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        self.transition(id, ComponentState::Stopping)
    }

    /// Stopping → Stopped：停止完成。
    ///
    /// 生产调用方是 `component/exit.rs::stop_component`，在
    /// `kcomp_instance_destroy` 返回 0 且 authority 兜底回收之后调用
    /// （唯一合法前驱是 `Stopping`）。
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

    /// 该实例拥有的任务是否允许运行。
    ///
    /// 只有活着的实例（`Starting` = `kcomp_instance_create` 执行期、`Ready`）
    /// 可以运行任务；`Failed`、`Stopping`、`Stopped` 实例的任务必须从 runnable
    /// 候选中剔除，并在 commit 前再次验证。
    pub fn may_run(&self, id: ComponentId) -> bool {
        self.get(id)
            .is_some_and(|r| matches!(r.state, ComponentState::Starting | ComponentState::Ready))
    }

    /// 开始一次 consumer→provider 调用记账（checked increment）。
    ///
    /// 只放行 `Ready` 实例（只有完整、已提交初始化的实例可服务调用；
    /// `Starting` = create 执行期，尚不可被消费）。溢出拒绝且不改计数。
    pub fn begin_call(&mut self, id: ComponentId) -> Result<(), RegistryError> {
        let record = self.record_mut(id)?;
        if record.state != ComponentState::Ready {
            return Err(RegistryError::NotReady);
        }
        record.inflight = record
            .inflight
            .checked_add(1)
            .ok_or(RegistryError::CallOverflow)?;
        Ok(())
    }

    /// 结束一次调用记账（decrement）。
    ///
    /// **不设生命周期门禁**：实例开始停止 / 失败后，已在飞行的调用仍须能归还
    /// 计数。未知 id 是 no-op；未配对的 finish 不下溢（记账错误不得变成 panic）。
    pub fn finish_call(&mut self, id: ComponentId) {
        if let Ok(record) = self.record_mut(id) {
            record.inflight = record.inflight.saturating_sub(1);
        }
    }

    /// 该实例当前在飞行的调用数；未知实例为 0。
    pub fn active_calls(&self, id: ComponentId) -> u32 {
        self.get(id).map_or(0, |record| record.inflight)
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

/// 测试专用：伪造一份已加载组件与声明辅助。**不分配 backing**（`memory` 为
/// `None`）——纯逻辑用例（状态机 / endpoint）不需要真实 lease；
/// 需要真实 backing 的用例走生产 loader 或自行 `alloc_region`。
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::component::containment::KCOMP_ABI;

    /// 一份伪造的 loaded component（`.text` 段 64 字节；入口 = base + 8）。
    pub(crate) fn test_loaded(destroy: usize, service_dispatch: Option<usize>) -> LoadedComponent {
        LoadedComponent {
            base: 0x1000,
            create: 0x1008,
            destroy,
            service_dispatch,
            text_size: 64,
            abi: KCOMP_ABI,
            memory: None,
        }
    }

    /// 一份伪造、**带真实常驻 backing** 的 loaded component（需要 region 的
    /// 用例用；调用方须持有 memory GUARD）。
    pub(crate) fn test_loaded_with_region(
        destroy: usize,
        service_dispatch: Option<usize>,
    ) -> LoadedComponent {
        let lease = crate::memory::alloc_region(crate::memory::ALLOC_GRANULE).unwrap();
        let base = lease.region().base;
        let size = lease.region().size;
        LoadedComponent {
            base,
            create: base + 8,
            destroy,
            service_dispatch,
            // 伪造镜像覆盖整块 region：`base..base+text_size` 是合法 entry 区间。
            text_size: size,
            abi: KCOMP_ABI,
            memory: Some(lease),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> Registry {
        Registry::new()
    }

    /// 声明一个测试组件（伪造 loaded image；调用方须持 memory GUARD）。
    fn declare(reg: &mut Registry, name: &[u8]) -> ComponentId {
        reg.declare(
            name,
            test_support::test_loaded(0, None),
            ExecutionDomain::KernelNative,
        )
        .unwrap()
    }

    /// 驱一个组件走完 init 路径到 `Ready`（stop 路径唯一合法的起点）。
    fn ready(reg: &mut Registry) -> ComponentId {
        let id = declare(reg, b"ready");
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    #[test]
    fn declare_assigns_increasing_ids() {
        let mut reg = r();
        let a = declare(&mut reg, b"a");
        let b = declare(&mut reg, b"b");
        assert_eq!(a.raw(), 1);
        assert_eq!(b.raw(), 2);
    }

    /// 每次 declare 得到的组件拥有**自己**的 loaded image（不是共享镜像）。
    #[test]
    fn each_component_owns_its_loaded_image() {
        let mut reg = r();
        let first = declare(&mut reg, b"same");
        let second = declare(&mut reg, b"same");
        assert_ne!(first, second);
        // 同名 artifact 两次 declare：各自一份独立 loaded image backing。
        let a = reg.get(first).unwrap();
        let b = reg.get(second).unwrap();
        assert_eq!(a.name, b"same");
        assert_eq!(b.name, b"same");
        // 这两份是测试替身（无 backing），但 loaded image 作为独立值各自持有。
        assert!(a.loaded.memory.is_none());
        assert!(b.loaded.memory.is_none());
        assert_eq!(reg.len(), 2);
    }

    /// 每个组件有**自己的** opaque state 指针；一个变不影响另一个。
    #[test]
    fn instance_state_is_per_instance() {
        let mut reg = r();
        let a = declare(&mut reg, b"a");
        let b = declare(&mut reg, b"b");
        let mut state_a = 1u8;
        let mut state_b = 2u8;
        let ptr_a = core::ptr::addr_of_mut!(state_a).cast::<()>();
        let ptr_b = core::ptr::addr_of_mut!(state_b).cast::<()>();

        // 出生时是 NULL（create 前的 Core 初始化值）。
        assert!(reg.get(a).unwrap().instance_state.is_null());
        reg.record_instance_state(a, ptr_a).unwrap();
        reg.record_instance_state(b, ptr_b).unwrap();
        assert_eq!(reg.get(a).unwrap().instance_state, ptr_a);
        assert_eq!(reg.get(b).unwrap().instance_state, ptr_b);

        // 无状态组件可记录 NULL；另一个组件的 state 不受影响。
        reg.record_instance_state(a, core::ptr::null_mut()).unwrap();
        assert!(reg.get(a).unwrap().instance_state.is_null());
        assert_eq!(reg.get(b).unwrap().instance_state, ptr_b);
    }

    #[test]
    fn record_instance_state_unknown_is_not_found() {
        let mut reg = r();
        assert_eq!(
            reg.record_instance_state(ComponentId::from_raw(99), core::ptr::null_mut()),
            Err(RegistryError::NotFound)
        );
    }

    /// 生命周期按组件独立推进：一个失败不改变另一个组件。
    #[test]
    fn lifecycle_is_per_component() {
        let mut reg = r();
        let a = declare(&mut reg, b"a");
        let b = declare(&mut reg, b"b");

        // b 在 init 中失败 → 只有 b 变成 Failed；a 仍可走完生命周期。
        reg.mark_failed(b).unwrap();
        assert!(reg.is_failed(b));
        assert!(!reg.is_failed(a));
        assert_eq!(reg.get(a).unwrap().state, ComponentState::Declared);

        reg.resolve(a).unwrap();
        reg.begin_start(a).unwrap();
        reg.finish_start(a).unwrap();
        assert!(reg.may_run(a));
        assert!(!reg.may_run(b), "Failed 组件不得运行任务");
        assert_eq!(reg.get(b).unwrap().state, ComponentState::Failed);
    }

    /// 对抗：非法转换被拒绝时，**真相不变**。
    ///
    /// 不断言"没有产生事件"：trace 只记录合法提交（`emit` 位于状态提交之后），
    /// 非法转换本就永远不会出现在事件流里，"事件为空"是不可证伪的断言。
    /// 真正有意义的是：被拒绝的尝试改不动真相。
    #[test]
    fn rejected_transition_leaves_truth_unchanged() {
        init();
        let mut reg = get_registry().lock();
        let id = declare(&mut reg, b"rejected");

        // Declared → Ready 非法（跳过 Resolved / Starting）。
        assert_eq!(reg.finish_start(id), Err(RegistryError::InvalidTransition));
        assert_eq!(
            reg.get(id).unwrap().state,
            crate::component::ComponentState::Declared,
            "被拒绝的转换不得改变真相"
        );

        // 其它非法边（Declared → Starting）同样不动真相。
        assert_eq!(reg.begin_start(id), Err(RegistryError::InvalidTransition));
        assert_eq!(
            reg.get(id).unwrap().state,
            crate::component::ComponentState::Declared
        );
    }

    #[test]
    fn is_failed_only_reports_failed_instances() {
        let mut reg = r();
        let id = declare(&mut reg, b"failed");
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

    /// `may_run` 与生命周期状态精确对应：Declared / Resolved 不跑工作，
    /// Starting / Ready 跑，Stopping / Stopped / Failed 与未知 id 不跑。
    #[test]
    fn may_run_matches_liveness_on_every_state() {
        let mut reg = r();
        let id = declare(&mut reg, b"may_run");
        assert!(!reg.may_run(id), "Declared does not run work");
        reg.resolve(id).unwrap();
        assert!(!reg.may_run(id), "Resolved does not run work");
        reg.begin_start(id).unwrap();
        assert!(
            reg.may_run(id),
            "Starting (kcomp_instance_create) may run work"
        );
        reg.finish_start(id).unwrap();
        assert!(reg.may_run(id), "Ready runs work");
        reg.begin_stop(id).unwrap();
        assert!(!reg.may_run(id), "Stopping must not run work");
        reg.finish_stop(id).unwrap();
        assert!(!reg.may_run(id), "Stopped must not run work");
        reg.mark_failed(id).unwrap();
        assert!(!reg.may_run(id), "Failed must not run work");
        assert!(
            !reg.may_run(ComponentId::from_raw(99)),
            "unknown must not run"
        );
    }

    #[test]
    fn tombstones_remain_visible_in_registry() {
        // 契约 §9：Stopped/Failed 记录不删除（tombstone），id 不复用。
        let mut reg = r();
        let stopped = ready(&mut reg);
        reg.begin_stop(stopped).unwrap();
        reg.finish_stop(stopped).unwrap();
        let failed = declare(&mut reg, b"failed_tombstone");
        reg.mark_failed(failed).unwrap();

        assert_eq!(reg.len(), 2, "tombstone 记录保留");
        assert_eq!(reg.get(stopped).unwrap().state, ComponentState::Stopped);
        assert_eq!(reg.get(failed).unwrap().state, ComponentState::Failed);

        // 新组件拿全新 id，不复用 tombstone 的 id。
        let fresh = declare(&mut reg, b"fresh_tombstone");
        assert_ne!(fresh, stopped);
        assert_ne!(fresh, failed);
        assert_eq!(fresh.raw(), failed.raw() + 1);
    }

    /// `begin_call` 溢出必须拒绝且不改计数。
    ///
    /// `inflight` 是 Registry 私有字段：同模块测试直接置位，避免 2^32 次真实调用。
    #[test]
    fn begin_call_overflow_is_rejected_without_changing_count() {
        let mut reg = r();
        let id = ready(&mut reg);
        reg.records
            .iter_mut()
            .find(|rec| rec.id == id)
            .unwrap()
            .inflight = u32::MAX;

        assert_eq!(reg.begin_call(id), Err(RegistryError::CallOverflow));
        assert_eq!(reg.active_calls(id), u32::MAX, "溢出拒绝不得改变计数");
    }

    // -- Property tests（生命周期状态机；docs/development/testing.md §2 / docs/architecture/component-model.md §5）--
    //
    // 对随机 declare / resolve / begin_start / finish_start / begin_stop /
    // finish_stop / mark_failed 序列，逐操作验证：
    //   1. 合法性精确：Ok ⟺ 当前状态按文档转移表放行；非法 → Err 且真相不变
    //   2. 不可复活：Stopped / Failed 之后任何成功操作都不得回到活状态
    //   3. declare 恒成功：组件数无上限，id 全局唯一且单调
    //   4. 精确前驱：resolve 仅自 Declared，其余各步仅自其文档前驱
    //   5. mark_failed 从任意状态可达且为终态
    //   6. 观察到的 registry 真相逐步等于模型
    //
    // 用局部 `Registry`（与其它测试同款 `Registry::new()`）：零全局状态，
    // 不碰 `Once<Mutex<Registry>>`，因此无需串行锁或唯一命名。

    use proptest::prelude::*;

    /// 文档转移表的**独立**副本（不调用 `ComponentState::can_transition`）——
    /// 作为 oracle 验证实现与文档一致，而非复述实现。
    fn is_legal(from: ComponentState, to: ComponentState) -> bool {
        use ComponentState::*;
        match (from, to) {
            (Declared, Resolved)
            | (Resolved, Starting)
            | (Starting, Ready)
            | (Ready, Stopping)
            | (Stopping, Stopped)
            | (_, Failed) => true, // 任意状态 → Failed（含 Failed → Failed 幂等）
            _ => false,
        }
    }

    /// 可被随机化的生命周期操作（每个对应一个 `Registry` 转换方法）。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum LifecycleOp {
        Resolve,
        BeginStart,
        FinishStart,
        BeginStop,
        FinishStop,
        MarkFailed,
    }

    impl LifecycleOp {
        fn target(self) -> ComponentState {
            match self {
                Self::Resolve => ComponentState::Resolved,
                Self::BeginStart => ComponentState::Starting,
                Self::FinishStart => ComponentState::Ready,
                Self::BeginStop => ComponentState::Stopping,
                Self::FinishStop => ComponentState::Stopped,
                Self::MarkFailed => ComponentState::Failed,
            }
        }

        fn apply(self, reg: &mut Registry, id: ComponentId) -> Result<(), RegistryError> {
            match self {
                Self::Resolve => reg.resolve(id),
                Self::BeginStart => reg.begin_start(id),
                Self::FinishStart => reg.finish_start(id),
                Self::BeginStop => reg.begin_stop(id),
                Self::FinishStop => reg.finish_stop(id),
                Self::MarkFailed => reg.mark_failed(id),
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Op {
        /// 声明一个新组件（同名不再拒绝；各自独立 loaded image）。
        Declare,
        /// 对一个"声明序号"引用实例执行生命周期操作；序号越界 = 未声明 id。
        Lifecycle { target: usize, op: LifecycleOp },
    }

    /// 可被引用的实例序号上限（大于典型序列中的成功声明数，以覆盖 NotFound）。
    const SLOTS: usize = 4;

    fn lifecycle_op_strategy() -> impl Strategy<Value = LifecycleOp> {
        prop_oneof![
            Just(LifecycleOp::Resolve),
            Just(LifecycleOp::BeginStart),
            Just(LifecycleOp::FinishStart),
            Just(LifecycleOp::BeginStop),
            Just(LifecycleOp::FinishStop),
            Just(LifecycleOp::MarkFailed),
        ]
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        let declare = Just(Op::Declare);
        let lifecycle = (0usize..SLOTS, lifecycle_op_strategy())
            .prop_map(|(target, op)| Op::Lifecycle { target, op });
        prop_oneof![declare, lifecycle]
    }

    /// 序列生成器：随机声明 + 随机生命周期操作的混合序列。
    fn op_seq() -> impl Strategy<Value = Vec<Op>> {
        proptest::collection::vec(op_strategy(), 1..=40)
    }

    /// 模型中的一条实例真相（`id == index + 1`：无 unload、id 单调不回收）。
    #[derive(Debug, Clone, Copy)]
    struct ModelRecord {
        id: ComponentId,
        state: ComponentState,
    }

    /// 测试本地模型：独立于 `Registry` 记录"应当"的真相。
    #[derive(Debug)]
    struct Model {
        records: alloc::vec::Vec<ModelRecord>,
        next_id: u32,
    }

    impl Model {
        fn new() -> Self {
            Self {
                records: alloc::vec::Vec::new(),
                next_id: 1,
            }
        }
    }

    /// 不变量 6：观察到的 registry 真相必须逐步等于模型；并检查 id 唯一/非哨兵。
    fn assert_model_matches(reg: &Registry, model: &Model) {
        assert_eq!(reg.len(), model.records.len(), "记录数须与模型一致");

        let observed: alloc::vec::Vec<(ComponentId, ComponentState)> =
            reg.iter().map(|r| (r.id, r.state)).collect();
        let expected: alloc::vec::Vec<(ComponentId, ComponentState)> =
            model.records.iter().map(|r| (r.id, r.state)).collect();
        assert_eq!(observed, expected, "观察真相须逐步等于模型");

        for (i, (id, _)) in expected.iter().enumerate() {
            assert_ne!(*id, ComponentId::from_raw(0), "id 不得为哨兵 0");
            for (j, (other, _)) in expected.iter().enumerate() {
                if i != j {
                    assert_ne!(id, other, "组件 id 必须唯一（不回收）");
                }
            }
        }
    }

    fn apply_and_check(model: &mut Model, reg: &mut Registry, op: Op) {
        match op {
            Op::Declare => {
                // When：声明一个新组件（同名不限组件数）。
                let result = reg.declare(
                    b"prop",
                    test_support::test_loaded(0, None),
                    ExecutionDomain::KernelNative,
                );

                // Then：分配下一单调 id，状态 Declared。
                let expected_id = ComponentId::from_raw(model.next_id);
                assert_eq!(result, Ok(expected_id), "declare 恒成功且 id 单调");
                assert_eq!(reg.get(expected_id).unwrap().name, b"prop");
                model.records.push(ModelRecord {
                    id: expected_id,
                    state: ComponentState::Declared,
                });
                model.next_id += 1;
            }
            Op::Lifecycle { target, op } => {
                // Given：序号对应的已声明实例（越界 → 哨兵 id，模型为"未声明"）。
                let declared = model.records.get(target).copied();
                let id = declared.map_or(ComponentId::from_raw(0), |r| r.id);
                let before = declared.map(|r| r.state);
                assert_eq!(
                    reg.get(id).map(|r| r.state),
                    before,
                    "操作前观察真相须等于模型"
                );

                // When：执行生命周期操作。
                let result = op.apply(reg, id);

                // Then 1：Ok ⟺ 文档转移表放行；未声明 → NotFound。
                let expected = match before {
                    None => Err(RegistryError::NotFound),
                    Some(from) if is_legal(from, op.target()) => Ok(()),
                    Some(_) => Err(RegistryError::InvalidTransition),
                };
                assert_eq!(result, expected, "{op:?} 自 {before:?} 的合法性");

                if result.is_ok() {
                    model.records[target].state = op.target();
                }

                if let Some(from) = before {
                    // Then 2：mark_failed 从任意状态可达（含 Stopped / Failed）。
                    if op == LifecycleOp::MarkFailed {
                        assert!(result.is_ok(), "mark_failed 须自 {from:?} 可达");
                    }
                    // Then 3：不可复活——Stopped / Failed 之后任何成功操作都
                    // 不得回到活状态（终态仅可幂等再标记 Failed）。
                    if result.is_ok()
                        && matches!(from, ComponentState::Stopped | ComponentState::Failed)
                    {
                        let to = model.records[target].state;
                        assert!(
                            !matches!(
                                to,
                                ComponentState::Declared
                                    | ComponentState::Resolved
                                    | ComponentState::Starting
                                    | ComponentState::Ready
                                    | ComponentState::Stopping
                            ),
                            "不可复活：{from:?} -> {to:?}"
                        );
                    }
                    // Then 4：非法操作不得改变真相。
                    if result.is_err() {
                        assert_eq!(
                            reg.get(id).map(|r| r.state),
                            Some(from),
                            "被拒绝的操作不得改变真相"
                        );
                    }
                }
            }
        }

        // 不变量 6：每步之后观察真相 == 模型。
        assert_model_matches(reg, model);
    }

    proptest! {
        #[test]
        fn random_lifecycle_sequences_match_state_machine(ops in op_seq()) {
            // Given：局部 Registry + 空模型（零全局状态；无需串行锁）。
            let mut reg = Registry::new();
            let mut model = Model::new();

            // When / Then：逐步执行并对每个操作验证全部不变量。
            for op in ops {
                apply_and_check(&mut model, &mut reg, op);
            }
        }
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
            for _ in 0..count {
                ids.push(declare(&mut reg, b"bench"));
            }
            let probe = ids[count / 2];
            let mut bench = crate::bench::Bench::new(name);
            bench.run(100, || reg.get(probe).is_some());
            bench.finish().report();
        }
    }
}
