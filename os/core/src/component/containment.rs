//! Component panic containment: create / task / destroy / service-call /
//! policy-call / cross-AS boundaries.
//!
//! A panic in component code never unwinds: the boundary switches control back
//! to a Core-owned frame, which classifies the outcome, marks the instance
//! `Failed`, and reschedules.  Compatible with `panic = "abort"`.
//!
//! - **Create / destroy** (`call_component_create` / `call_component_destroy`):
//!   the hook runs on a temporary Core-owned stack; a panic returns to
//!   `component/exit.rs::stop_component` as a destroy failure.
//! - **Task** (`enter_task` → `task_abort_trampoline`): a panicking task is
//!   committed to `Exited`, fails its owning component, and reschedules in a
//!   clean Core context.
//! - **Service call** (`call_component_service`, `kcore_endpoint_call`): the
//!   provider's `kcomp_service_dispatch` runs on a Core-owned stack under the
//!   **provider's** principal (`ambient()` = provider, `task` = caller as
//!   provenance only; `ambient_init()` is `None`).  A panic is failed by
//!   `component/call.rs`, never by the caller.  [`scheduling_forbidden`]
//!   rejects `sched::run` / yield / exit / task creation for the whole chain
//!   ([`provider_in_active_chain`] rejects re-entry), and interrupt-enable
//!   state is saved/restored explicitly around the switch (the RISC-V context
//!   record does not carry `sstatus.SIE`; `tp` **is** part of it, as plain
//!   architectural execution state).
//! - **Policy call** (`crate::sched` → `call_component_policy`):
//!   Core is the caller (no principal, no caller task); a panic returns to the
//!   suspended scheduler frame, which still owns its `IrqSaveGuard`.
//! - **Cross-AS service** (`with_isolated_service_boundary`; KernelNative
//!   caller → Isolated provider): enters the provider's private AS through the
//!   minimal cross-AS trampoline with the same `EscapeKind::ServiceCall`
//!   bookkeeping; a provider fault is contained by the ordinary Core trap path
//!   (`Outcome::Faulted` via the cross-AS recoverable context), and a provider
//!   panic escapes through the same cross-AS context (`kcore_panic_escape`),
//!   not through this guard's (null) Core context.
//!
//! # Escapability gate (Core ABI depth)
//!
//! Escaping a panic from Core code (an ordinary `kcore_*` export body) would
//! resume `fail_component`'s recovery path with Core locks still held →
//! **deadlock**.  Every export body therefore runs inside [`with_core_critical`]
//! (depth +1), except the explicit escape request `kcore_panic_escape`; every
//! boundary handing control to component code ([`run_isolated`],
//! [`with_irq_scope`], [`enter_task`]) suspends the depth, and task switches
//! save/restore it in the suspended scheduler frame.  A depth refusal leaves
//! the guard untouched — the panic stays fatal.
//!
//! # Execution guard (lock-free)
//!
//! The CPU-local active guard is a plain static pointer, never a lock: the boot panic
//! handler performs no allocation, logging, or locking.  It is only touched at
//! synchronous entry/switch boundaries on the current CPU.  [`with_irq_scope`]
//! layers one more boundary around a component IRQ callback: principal is the
//! IRQ line's owner, `task = None`, scheduling is forbidden beneath it, and it
//! is **not escapable** (an IRQ callback has no Core-owned context to resume).
//! It is bookkeeping for trusted KernelNative components, not an authentication
//! boundary.
//!
//! # Retained on failure (non-obvious)
//!
//! Image, allocations, abandoned stack frames and panicked service/policy
//! stacks stay resident (`mem::forget`, no allocator lock on the panic path);
//! only authority is revoked via `fail_component`.  Tasks of a failed component
//! are excluded by `may_run` but keep non-`Exited` records (no task-stop API),
//! so graceful stop refuses components that still own live tasks.  No Core lock
//! may span the switch.  The task-abort context is per-CPU, entered once per
//! panic, never resumed.
//!
//! Contract: `docs/modules/core/component.md`; `docs/philosophy/core-philosophy.md`
//! §5.8.

use crate::component::ComponentId;
use crate::component::endpoint::EndpointId;
use crate::generated::abi::KcompCallFrame;
use crate::memory::{self, MemoryLease};
use crate::task::TaskId;
use arch::{ContextImpl, CpuArch, CpuImpl};
use core::mem::MaybeUninit;

const COMPONENT_STACK_BYTES: usize = 32 * 1024;
const TASK_ABORT_STACK_BYTES: usize = 32 * 1024;
const STACK_ALIGNMENT: usize = 16;
/// 边界栈分配失败时报告的组件入口状态码（`-ENOMEM`）。
///
/// 这是 **Core 侧**失败（组件入口从未执行）：类型化表达是
/// [`CallOutcome::NoStack`]；本常量只供生命周期编排（`component/load.rs` /
/// `component/exit.rs`）把 `NoStack` 归入既有的 `CreateFailed` /
/// `DestroyFailed` 分类。
pub(crate) const STACK_ALLOCATION_FAILED: i32 = -12;

// ---------------------------------------------------------------------------
// 组件实例 ABI（C 是根，见 `docs/architecture/component-lifecycle.md` §4）
// ---------------------------------------------------------------------------

pub use crate::generated::abi::{KCOMP_ABI, KcompCreateArgs};

impl KcompCreateArgs {
    /// 无配置负载（`kcore_component_load` 的默认配置）。
    pub const fn empty() -> Self {
        Self {
            config_abi: 0,
            config: core::ptr::null(),
            config_len: 0,
        }
    }
}

/// `kcomp_instance_create(args, out_state) -> 0 / -errno`。
/// Core 先把 `*out_state` 置 NULL；组件成功时写入自己的 state 指针
/// （**无状态组件允许写 NULL**）。返回非零失败。
type InstanceCreate = extern "C" fn(args: *const KcompCreateArgs, out_state: *mut *mut ()) -> i32;

/// `kcomp_instance_destroy(state) -> 0 / -errno`。
type InstanceDestroy = extern "C" fn(state: *mut ()) -> i32;

/// `kcomp_service_dispatch(state, port, method, frame) -> 0 / -errno` 的 Core 侧
/// 函数类型（手写镜像 `abi/component.toml` 的 `KcompServiceDispatch`，与
/// `InstanceCreate` / `InstanceDestroy` 同款）。`call.rs` 在锁内解析后把地址拷进
/// [`IsolatedCall::Service`]，由 [`trampoline`] 在 service 栈上调用。
pub(crate) type ServiceDispatch = extern "C" fn(*mut (), u32, u32, *const KcompCallFrame) -> i32;

/// 隔离栈上要执行的一次组件调用（参数由调用方在 Core 栈帧里携带）。
#[derive(Clone, Copy)]
enum IsolatedCall {
    /// 测试边界 / 无组件调用。
    None,
    Create {
        entry: usize,
        args: *const KcompCreateArgs,
        out_state: *mut *mut (),
    },
    Destroy {
        entry: usize,
        state: *mut (),
    },
    /// 一次组件 dispatcher 调用（provider 的 `kcomp_service_dispatch`）：service
    /// call 与 policy call 共用同一调用形状，参数全部来自 `component/call.rs`
    /// （service）或 `sched` 的锁内快照（policy）的拷贝。
    Service {
        dispatcher: ServiceDispatch,
        instance_state: *mut (),
        port: u32,
        method: u32,
        frame: *const KcompCallFrame,
    },
}

/// Result of invoking a component entry on its isolated stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    Returned(i32),
    Panicked,
    /// The Core-owned boundary stack could not be allocated: the component entry
    /// **never ran**.  This is a Core-side failure (`-ENOMEM`), not a component
    /// status — it exists so `Returned(i32)` can never be confused with the
    /// boundary failing to start.
    NoStack,
}

/// Which Core boundary an active escape guard protects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeKind {
    /// `kcomp_instance_create` running on a temporary Core stack.  The owner is
    /// the component Core is initializing, or `None` for a direct (selftest) call.
    Init { owner: Option<ComponentId> },
    /// `kcomp_instance_destroy` running on a temporary Core stack during a
    /// graceful stop.
    ///
    /// The owner is the **instance being stopped** — never the monitor or other
    /// component that initiated the stop — so identity-sensitive Core calls made
    /// by the hook are attributed to the stopped instance.
    Exit { owner: ComponentId },
    /// A component task running on its own kernel stack.
    Task { task: TaskId, owner: ComponentId },
    /// A component IRQ callback running synchronously on the trap path
    /// ([`crate::irq::on_irq`]).
    ///
    /// The owner is the **IRQ line's owner** (Core truth from the routing
    /// table), never the interrupted execution; there is no task because an
    /// IRQ callback is not a task.  Installed by [`with_irq_scope`], which is
    /// cooperative bookkeeping — not an authentication boundary — and is
    /// neither yielding nor escapable (see the module docs).
    Irq { owner: ComponentId },
    /// One component→component service call running on its own Core-owned
    /// service stack ([`call_component_service`]).
    ///
    /// The owner is the **provider** (Core truth from the resolved endpoint),
    /// so Core calls the dispatcher makes are attributed to the provider.
    /// `caller_task` is the task the call originated from — **execution
    /// provenance, not authorization** to act as that task's owner; it is
    /// `None` when the call was made from a non-task boundary (init / exit).
    /// The boundary is escapable (unlike `Irq`) and scheduling-forbidden (see
    /// the module docs).
    ServiceCall {
        owner: ComponentId,
        endpoint: EndpointId,
        caller_task: Option<TaskId>,
    },
    /// One **scheduler policy call** running on the policy stack Core prepared
    /// when the policy endpoint was selected ([`call_component_policy`]).
    ///
    /// The caller is **Core itself** (the scheduling commit path), so there is
    /// no caller task: the owner is the policy provider (Core truth from the
    /// resolved endpoint) and the boundary carries no task provenance — a
    /// policy panic must not inherit the boundary of the task that yielded.
    /// The boundary is escapable (a panic returns to the suspended scheduler
    /// frame, which still owns its `IrqSaveGuard`), scheduling-forbidden, and
    /// additionally forbids generic endpoint calls, nested component creation,
    /// and policy replacement ([`policy_call_in_chain`]).
    PolicyCall {
        owner: ComponentId,
        endpoint: EndpointId,
    },
}

impl EscapeKind {
    /// Whether a panic in this boundary escapes to the Core-owned context
    /// recorded in the guard ([`panic_escape`]).
    ///
    /// An IRQ attribution scope has no Core-owned context to resume and must
    /// stay fatal; every other boundary (init / exit / task / service call /
    /// policy call) was entered through a saved Core context and can escape to
    /// it.
    pub(crate) const fn is_escapable(self) -> bool {
        !matches!(self, Self::Irq { .. })
    }
}

/// Lock-free snapshot of the active escape guard, for boot diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscapeInfo {
    pub kind: EscapeKind,
}

impl EscapeInfo {
    /// Owning component, when known (`Task` / `Exit` / `Irq` / `ServiceCall` /
    /// `PolicyCall` always, `Init` only inside a load).
    pub fn owner(self) -> Option<ComponentId> {
        match self.kind {
            EscapeKind::Init { owner } => owner,
            EscapeKind::Exit { owner } => Some(owner),
            EscapeKind::Task { owner, .. } => Some(owner),
            EscapeKind::Irq { owner } => Some(owner),
            EscapeKind::ServiceCall { owner, .. } => Some(owner),
            EscapeKind::PolicyCall { owner, .. } => Some(owner),
        }
    }

    /// Running task id, or `None` at the init, exit, and IRQ boundaries.
    ///
    /// A service call reports the **caller task it originated from** (execution
    /// provenance); it does not make the provider that task's owner.  A policy
    /// call has **no** task: Core is the caller, and the boundary must not
    /// inherit the yielding task's identity.
    pub fn task(self) -> Option<TaskId> {
        match self.kind {
            EscapeKind::Init { .. } | EscapeKind::Exit { .. } | EscapeKind::Irq { .. } => None,
            EscapeKind::Task { task, .. } => Some(task),
            EscapeKind::ServiceCall { caller_task, .. } => caller_task,
            EscapeKind::PolicyCall { .. } => None,
        }
    }
}

/// Nesting state independent of the architecture-specific escape records.
#[derive(Debug, Clone, Copy)]
struct GuardState<T> {
    previous: Option<T>,
    panicked: bool,
}

impl<T: Copy> GuardState<T> {
    const fn new(previous: Option<T>) -> Self {
        Self {
            previous,
            panicked: false,
        }
    }

    const fn previous(self) -> Option<T> {
        self.previous
    }

    fn mark_panicked(&mut self) {
        self.panicked = true;
    }

    const fn panicked(self) -> bool {
        self.panicked
    }
}

/// One ambient escape record.  `from_context` receives the escaping execution's
/// register state; `to_context` is the Core-owned context resumed instead.
struct EscapeGuard {
    kind: EscapeKind,
    from_context: *mut ContextImpl,
    to_context: *mut ContextImpl,
    /// 隔离栈上要执行的那次组件调用（测试边界为 [`IsolatedCall::None`]）。
    call: IsolatedCall,
    returned: i32,
    state: GuardState<*mut EscapeGuard>,
    /// Core ABI 深度在边界安装时被挂起（[`suspend_core_abi_depth`]），控制回到
    /// 安装该 guard 的 Core 帧（正常返回或 panic 逃逸）时恢复。
    ///
    /// 任务边界不用它：任务 guard 每次切换都被覆盖，被恢复任务的深度由它自己
    /// 挂起的调度帧（`sched::schedule_next`）保存/恢复。
    saved_depth: u32,
}

/// Execution state belongs to the current CPU; it is never restored from a task.
/// Raw field accesses are short and no reference spans an IRQ or context switch.
struct CpuContainment {
    active: *mut EscapeGuard,
    anchor: *mut EscapeGuard,
    task_region: bool,
    #[cfg(not(test))]
    depth: u32,
    creating: Option<ComponentId>,
    abort_stack: TaskAbortStack,
    scratch: MaybeUninit<ContextImpl>,
    abort: MaybeUninit<ContextImpl>,
    task_guard: MaybeUninit<EscapeGuard>,
    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64", test))]
    cross_as: *mut cross_as::CrossAsContext,
}

impl CpuContainment {
    const fn new() -> Self {
        Self {
            active: core::ptr::null_mut(),
            anchor: core::ptr::null_mut(),
            task_region: false,
            #[cfg(not(test))]
            depth: 0,
            creating: None,
            abort_stack: TaskAbortStack([0; TASK_ABORT_STACK_BYTES]),
            scratch: MaybeUninit::uninit(),
            abort: MaybeUninit::uninit(),
            task_guard: MaybeUninit::uninit(),
            #[cfg(any(target_arch = "riscv32", target_arch = "riscv64", test))]
            cross_as: core::ptr::null_mut(),
        }
    }
}

struct CpuSlot(core::cell::UnsafeCell<CpuContainment>);
// SAFETY: each slot is accessed only on its CPU, without migration. Panic/IRQ
// paths use raw accesses and never keep a mutable reference to the slot alive.
unsafe impl Sync for CpuSlot {}
static LOCAL: [CpuSlot; crate::machine::MAX_CPUS] =
    [const { CpuSlot(core::cell::UnsafeCell::new(CpuContainment::new())) };
        crate::machine::MAX_CPUS];

fn local() -> *mut CpuContainment {
    let cpu = CpuImpl::current_cpu().unwrap_or(crate::machine::CpuId::from_raw(0));
    LOCAL[cpu.raw()].0.get()
}

// Host test threads each simulate an execution; their ABI depth is independent.
#[cfg(test)]
std::thread_local! {
    static CORE_ABI_DEPTH: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}

pub(crate) fn core_abi_depth() -> u32 {
    #[cfg(not(test))]
    // SAFETY: a short read of the current CPU's field.
    return unsafe { core::ptr::addr_of!((*local()).depth).read() };
    #[cfg(test)]
    return CORE_ABI_DEPTH.with(core::cell::Cell::get);
}

fn set_core_abi_depth(next: u32) {
    #[cfg(not(test))]
    // SAFETY: a short write of the current CPU's field.
    unsafe {
        core::ptr::addr_of_mut!((*local()).depth).write(next)
    };
    #[cfg(test)]
    CORE_ABI_DEPTH.with(|depth| depth.set(next));
}

/// CPU-local lifecycle attribution, saved/restored by nested create calls.
pub(crate) fn creating() -> Option<ComponentId> {
    // SAFETY: only this CPU reads/writes its current create identity.
    unsafe { core::ptr::addr_of!((*local()).creating).read() }
}

pub(crate) fn replace_creating(next: Option<ComponentId>) -> Option<ComponentId> {
    // SAFETY: synchronous CPU-local push/pop; never held across a reference.
    unsafe { core::ptr::replace(core::ptr::addr_of_mut!((*local()).creating), next) }
}

/// Runs `f` as **Core-critical**: the Core ABI depth is incremented for the whole
/// call, so a panic inside cannot escape into containment — Core code may hold
/// Core locks, and the escape's recovery path would deadlock on them.
///
/// Every ordinary `kcore_*` export body is wrapped here.  The one exception is
/// the SDK's explicit escape request `kcore_panic_escape`: wrapping it would
/// make the escape request itself permanently non-escapable.
pub(crate) fn with_core_critical<R>(f: impl FnOnce() -> R) -> R {
    set_core_abi_depth(core_abi_depth() + 1);
    let result = f();
    // Explicit decrement, no `Drop` (there is no unwinding): a panic either
    // escapes (control never returns to this frame) or stays fatal.
    set_core_abi_depth(core_abi_depth() - 1);
    result
}

/// Suspends the Core ABI depth for a component boundary: saves the current value
/// and zeroes it, so the component code about to run is escapable.  The saved
/// value is restored by [`resume_core_abi_depth`] when control returns to the
/// installing Core frame.  Not used at the scheduler boundary — see
/// [`EscapeGuard::saved_depth`].
fn suspend_core_abi_depth() -> u32 {
    let saved = core_abi_depth();
    set_core_abi_depth(0);
    saved
}

/// Restores a depth saved by [`suspend_core_abi_depth`].
pub(crate) fn resume_core_abi_depth(saved: u32) {
    set_core_abi_depth(saved);
}

/// Dedicated stack for the task-abort trampoline: it runs after the dead task's
/// stack is abandoned, so it must have its own.
#[repr(align(16))]
struct TaskAbortStack([u8; TASK_ABORT_STACK_BYTES]);
/// Initialize this CPU's executable panic destination before it goes Online.
pub fn init() {
    let cpu = CpuImpl::current_cpu().unwrap_or(crate::machine::CpuId::from_raw(0));
    init_cpu(cpu).expect("containment CPU initialization failed");
}

pub(crate) fn init_cpu(cpu: crate::machine::CpuId) -> Result<(), ContainmentInitError> {
    if CpuImpl::current_cpu().unwrap_or(crate::machine::CpuId::from_raw(0)) != cpu {
        return Err(ContainmentInitError::WrongCpu);
    }
    let slot = local();
    // SAFETY: this CPU is not yet Online; static backing is already mapped in
    // the kernel image and remains resident. No other CPU touches this slot.
    unsafe {
        let top = (core::ptr::addr_of_mut!((*slot).abort_stack.0) as usize
            + TASK_ABORT_STACK_BYTES)
            & !(STACK_ALIGNMENT - 1);
        let context = CpuImpl::new_context(task_abort_trampoline as *const () as usize, top);
        core::ptr::addr_of_mut!((*slot).abort).write(MaybeUninit::new(context));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainmentInitError {
    WrongCpu,
}

fn replace_active(next: *mut EscapeGuard) -> Option<*mut EscapeGuard> {
    // SAFETY: [Category 2 — Data races] one writer on this CPU; this pointer is
    // changed only at synchronous entry/return boundaries; the
    // panic handler runs on that same CPU and performs no nested mutation.
    let previous = unsafe { core::ptr::replace(core::ptr::addr_of_mut!((*local()).active), next) };
    (!previous.is_null()).then_some(previous)
}

fn active_guard() -> Option<*mut EscapeGuard> {
    // SAFETY: [Category 2 — Data races] the CPU-local contract used by
    // `replace_active` also serializes this raw read with guard installation.
    let guard = unsafe { core::ptr::addr_of!((*local()).active).read() };
    (!guard.is_null()).then_some(guard)
}

// ---------------------------------------------------------------------------
// Cross-AS recoverable context (Isolated execution)
// ---------------------------------------------------------------------------

/// 跨 AS（Isolated）执行的可恢复现场：只在真实 RISC-V 目标（`isolated::enter`
/// 消费）与 host 测试（LIFO 链）编译。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", test))]
pub(crate) mod cross_as {
    /// 一次 Isolated 执行的**可恢复现场**：普通 Core trap 路径上的异常钩子据此
    /// 判定“这次异常属于一个仍挂起的跨 AS 调用”，并能在放弃时交回进入前的 Core
    /// 调用者。
    ///
    /// `context` / `abandon` 由 arch 的 trampoline 提供（类型擦除：containment 是
    /// host-testable 的 Core 模块，不依赖 RISC-V 类型）。记录本身**每次调用独立**
    /// （在 `isolated::enter` 的栈帧里）；这里只保存指向当前记录的指针，按 LIFO
    /// 嵌套恢复，因此未来 A→B 的直接切换不需要单例 scratch。
    pub(crate) struct CrossAsContext {
        /// trampoline 的每次调用记录（arch 私有布局）。
        pub context: *mut u8,
        /// 放弃被打断的执行并交回挂起的 Core 调用者；永不返回。
        pub abandon: unsafe fn(*mut u8) -> !,
        /// 被打断执行应处的 `satp`（不匹配 = 不是这次 Isolated 调用的故障）。
        pub expected_satp: usize,
        /// 被打断执行所属实例的地址空间（组件私有可执行范围判定）。
        pub space: Option<crate::memory::address_space::AddressSpaceHandle>,
    }

    /// 安装本次 Isolated 执行的可恢复现场，返回被替换的上一个（LIFO / 嵌套安全）。
    pub(crate) fn swap_cross_as(next: *mut CrossAsContext) -> *mut CrossAsContext {
        // SAFETY: [Category 2 — Data races] CPU-local; only touched at
        // synchronous Isolated entry/return boundaries.
        unsafe { core::ptr::replace(core::ptr::addr_of_mut!((*super::local()).cross_as), next) }
    }

    /// 恢复 [`swap_cross_as`] 返回的上一个现场。
    pub(crate) fn restore_cross_as(previous: *mut CrossAsContext) {
        // SAFETY: same CPU-local contract as `swap_cross_as`.
        unsafe { core::ptr::addr_of_mut!((*super::local()).cross_as).write(previous) };
    }

    /// 当前 Isolated 执行的可恢复现场；`None` = 当前不在 Isolated AS 里执行。
    pub(crate) fn active_cross_as() -> Option<*mut CrossAsContext> {
        // SAFETY: [Category 2 — Data races] same CPU-local contract.
        let ctx = unsafe { core::ptr::addr_of!((*super::local()).cross_as).read() };
        (!ctx.is_null()).then_some(ctx)
    }
}

/// Reads the active escape guard without locks or allocation.  Returns `None`
/// when the current execution is not inside a component boundary.
pub fn active_escape() -> Option<EscapeInfo> {
    let guard = active_guard()?;
    // SAFETY: [Category 2 — Data races] `guard` points at a live record on this
    // CPU; `EscapeKind` is `Copy`, so this is a plain register-width read.
    let kind = unsafe { (*guard).kind };
    Some(EscapeInfo { kind })
}

/// Whether the **innermost** active Core-managed boundary is an IRQ callback
/// ([`EscapeKind::Irq`]).
///
/// The production gates use the ancestor-aware [`scheduling_forbidden`] instead
/// (a nested boundary on top of an IRQ scope must not hide it), so this
/// top-guard query is kept **for tests only** — it pins the IRQ-specific
/// behavior independently of the chain walk.
#[cfg(test)]
pub(crate) fn in_irq_context() -> bool {
    matches!(
        active_escape().map(|info| info.kind),
        Some(EscapeKind::Irq { .. })
    )
}

/// Walks the active boundary chain (innermost → outermost) and reports whether
/// **any** guard satisfies `predicate`.
///
/// The walk follows each guard's saved predecessor and deliberately stops at
/// the scheduler anchor: [`enter_task`] installs a fresh task guard with no
/// predecessor, so the anchor's guard (e.g. an enclosing init boundary) is not
/// part of a task's synchronous chain — a task's chain must not treat the anchor
/// as a service predecessor.
///
/// Lock-free; the chain is only walked synchronously, while every guard's owner
/// frame is suspended on this CPU.
fn chain_any(predicate: impl Fn(&EscapeGuard) -> bool) -> bool {
    let mut next = active_guard();
    while let Some(guard_ptr) = next {
        // SAFETY: [Category 2 — Data races] CPU-local; the chain is
        // stable while this synchronous walk runs (no guard is popped
        // concurrently), and every `previous` points at a live suspended frame.
        let guard = unsafe { &*guard_ptr };
        if predicate(guard) {
            return true;
        }
        next = guard.state.previous();
    }
    false
}

/// Whether **any** boundary in the active chain is an IRQ attribution scope
/// ([`EscapeKind::Irq`]) — including one hidden beneath nested lifecycle
/// boundaries.
pub(crate) fn irq_in_chain() -> bool {
    chain_any(|guard| matches!(guard.kind, EscapeKind::Irq { .. }))
}

/// Whether **any** boundary in the active chain forbids scheduling: an IRQ scope
/// (synchronous, non-yielding top half), a service call (the provider runs on
/// a Core-owned service stack under its own principal — there is no
/// scheduler-visible task, and switching away would abandon the service stack),
/// or a policy call (the scheduler frame itself is suspended; re-entering the
/// scheduler from a policy callback would corrupt that frame).
///
/// Ancestor-aware by design: a nested init / exit / task boundary on top of an
/// IRQ or service boundary must not re-open the scheduler
/// (`Service → create → sched::run` is rejected).  The Core mechanisms
/// (`sched::run` / `yield_current` / `exit_current`, `task::create_task` /
/// `start_task`) consult this and return an errno instead of panicking.
pub(crate) fn scheduling_forbidden() -> bool {
    chain_any(|guard| {
        matches!(
            guard.kind,
            EscapeKind::Irq { .. } | EscapeKind::ServiceCall { .. } | EscapeKind::PolicyCall { .. }
        )
    })
}

/// Whether **any** boundary in the active chain is a policy call
/// ([`EscapeKind::PolicyCall`]) — including one hidden beneath nested lifecycle
/// boundaries.
///
/// The policy callback is bounded Core-side: while it runs (or anywhere beneath
/// it), the generic endpoint call path, nested component creation, and policy
/// replacement are all rejected.  Ancestor-aware for the same reason as
/// [`scheduling_forbidden`]: a nested boundary must not hide the policy
/// execution.
pub(crate) fn policy_call_in_chain() -> bool {
    chain_any(|guard| matches!(guard.kind, EscapeKind::PolicyCall { .. }))
}

/// Whether `provider` already runs in the active synchronous chain: as the owner
/// of a task guard, as the owner of any service-call guard, as the policy
/// provider of a policy call, or as the instance being created / destroyed by an
/// enclosing init / exit guard.
///
/// `component/call.rs` consults this before dispatching: a synchronous call back
/// into an instance that is already on the current chain is re-entry, not a
/// service request.  IRQ scopes are intentionally not counted — an IRQ callback
/// cannot reach a generic service call at all (the IRQ chain gate rejects it
/// first).
pub(crate) fn provider_in_active_chain(provider: ComponentId) -> bool {
    chain_any(|guard| match guard.kind {
        EscapeKind::Task { owner, .. }
        | EscapeKind::ServiceCall { owner, .. }
        | EscapeKind::PolicyCall { owner, .. } => owner == provider,
        EscapeKind::Init { owner: Some(owner) } | EscapeKind::Exit { owner } => owner == provider,
        EscapeKind::Init { owner: None } | EscapeKind::Irq { .. } => false,
    })
}

/// Calls a component **create entry** (`kcomp_instance_create`) on a Core-owned
/// stack, under the identity of the instance Core is initializing
/// (`load::current_component`, which also covers nested loads).
///
/// `out_state` must point at caller-owned storage; Core's caller is required to
/// have initialized it to NULL before this call (the component may legally write
/// NULL on success for a stateless component).
pub fn call_component_create(
    entry: usize,
    args: *const KcompCreateArgs,
    out_state: *mut *mut (),
) -> CallOutcome {
    // 契约 §4：Core 先把 `*out_state` 置 NULL（由 Core 调用方 = `load.rs` 在
    // 传入前完成）；组件成功时写入自己的 state（无状态组件可保持 NULL）。
    // SAFETY: `loader::load_component` validates and relocates
    // `kcomp_instance_create` before its entry address reaches this Core-only
    // function; `args` / `out_state` live in the suspended caller frame.
    call_on_isolated_stack_with(
        IsolatedCall::Create {
            entry,
            args,
            out_state,
        },
        EscapeKind::Init {
            owner: crate::component::load::current_component(),
        },
    )
}

/// Calls a component **destroy entry** (`kcomp_instance_destroy`) on a
/// Core-owned stack.
///
/// The guard records `owner` — the instance being stopped — as the ambient
/// identity, so `RequestContext::ambient()` inside the hook resolves to that
/// instance (never to the monitor/caller that initiated the stop, and never to
/// `load::current_component()`).  Panic routing is the same as create: the
/// escape switches back to `stop_component`, which classifies the outcome.
pub fn call_component_destroy(entry: usize, state: *mut (), owner: ComponentId) -> CallOutcome {
    // SAFETY: `entry` comes from the component's own loaded image
    // (`ComponentRecord.loaded.destroy`), which the loader resolved and
    // relocated from the component's symbol table (same contract as
    // `call_component_create`).
    call_on_isolated_stack_with(
        IsolatedCall::Destroy { entry, state },
        EscapeKind::Exit { owner },
    )
}

/// Calls a component **service dispatcher** (`kcomp_service_dispatch`) on its own
/// Core-owned **service stack**, under the provider's principal
/// ([`EscapeKind::ServiceCall`]).
///
/// This is the execution boundary of `kcore_endpoint_call`
/// (`component/call.rs`): the provider's dispatcher runs on a per-call 32 KiB
/// Core-owned stack, so a panic switches back to the suspended caller frame
/// instead of unwinding into (and killing) the caller.  `owner` is the resolved
/// provider, `endpoint` its opaque endpoint identity (diagnostics), and
/// `caller_task` the task the call originated from — **provenance only**, never
/// authority over that task.
///
/// The whole invocation is an irq-save critical section: the RISC-V context
/// record does not carry `sstatus.SIE`, so Core saves the caller's
/// interrupt-enable state and restores it after the switch — no trap lands on
/// the freshly-installed service stack, and a provider that escapes while
/// holding an irq-save (or leaves `SIE` cleared) cannot strand Core with
/// interrupts disabled.  A nested service call observes `SIE = 0` and its
/// restore is a no-op.
///
/// Stack lifecycle: a normal return frees the service stack; a panic **retains**
/// it (explicit `mem::forget` of the lease, and no allocator lock on the panic
/// path) — phase-1 conservative residency, exactly like the failed component's
/// image and abandoned frames.
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_component_service(
    owner: ComponentId,
    endpoint: EndpointId,
    caller_task: Option<TaskId>,
    dispatcher: ServiceDispatch,
    instance_state: *mut (),
    port: u32,
    method: u32,
    frame: *const KcompCallFrame,
) -> CallOutcome {
    let irq_flags = CpuImpl::disable_irq();
    let run = run_isolated(
        IsolatedCall::Service {
            dispatcher,
            instance_state,
            port,
            method,
            frame,
        },
        EscapeKind::ServiceCall {
            owner,
            endpoint,
            caller_task,
        },
    );
    CpuImpl::restore_irq(irq_flags);

    let IsolatedRun { outcome, stack } = run;
    match (outcome, stack) {
        (CallOutcome::Panicked, Some(stack)) => {
            core::mem::forget(stack);
            CallOutcome::Panicked
        }
        (outcome, Some(stack)) => {
            let _ = memory::free_region(stack);
            outcome
        }
        (_, None) => CallOutcome::NoStack,
    }
}

/// Cross-AS (Isolated provider) service-call **identity boundary**: installs one
/// [`EscapeKind::ServiceCall`] guard around `f` with exactly the same principal
/// / provenance / re-entry / scheduling-forbidden discipline as
/// [`call_component_service`], but **no Core stack switch** — the execution
/// switch is the cross-AS trampoline's (`isolated::enter`), and `f` performs it.
///
/// The guard's context records are deliberately null: the cross-AS execution
/// owns its recoverable context (`cross_as::CrossAsContext`), so an Isolated
/// panic or fault is contained there — never by resuming a Core context through
/// this guard.  A provider **fault** is contained by the ordinary Core trap path
/// (`Outcome::Faulted` → the cross-AS recoverable context), and a provider
/// **panic** (`kcore_panic_escape`) escapes through the same cross-AS context.
/// [`escape_target`] refuses an escape through a guard with a null context
/// (defensive: stay fatal rather than switch to a null context).
///
/// Callers must not hold any Core lock across `f` (the trampoline requires the
/// same).
// 只被 Isolated dispatch 路径（`isolated_lifecycle`）调用；host 构建没有 Isolated
// 实例，但 host 用例（`isolated_service_boundary_has_no_escapable_core_context`）
// 直接驱动本边界。
#[allow(dead_code)]
pub(crate) fn with_isolated_service_boundary<R>(
    owner: ComponentId,
    endpoint: EndpointId,
    caller_task: Option<TaskId>,
    f: impl FnOnce() -> R,
) -> R {
    with_cross_as_boundary(
        EscapeKind::ServiceCall {
            owner,
            endpoint,
            caller_task,
        },
        f,
    )
}

/// Cross-AS create/destroy must replace the caller's identity and suspend its
/// Core ABI depth, just as native lifecycle boundaries do. The trampoline owns
/// fault recovery; this guard only restores identity and depth after it returns.
#[allow(dead_code)]
pub(crate) fn with_isolated_create_boundary<R>(owner: ComponentId, f: impl FnOnce() -> R) -> R {
    with_cross_as_boundary(EscapeKind::Init { owner: Some(owner) }, f)
}

#[allow(dead_code)]
pub(crate) fn with_isolated_destroy_boundary<R>(owner: ComponentId, f: impl FnOnce() -> R) -> R {
    with_cross_as_boundary(EscapeKind::Exit { owner }, f)
}

fn with_cross_as_boundary<R>(kind: EscapeKind, f: impl FnOnce() -> R) -> R {
    let mut guard = EscapeGuard {
        kind,
        // 无 Core 栈切换；可恢复的上下文由 cross-AS trampoline 持有。
        from_context: core::ptr::null_mut(),
        to_context: core::ptr::null_mut(),
        call: IsolatedCall::None,
        returned: 0,
        state: GuardState::new(None),
        // 与 `call_component_service` 同一纪律：组件代码从 depth 0 开始。
        saved_depth: suspend_core_abi_depth(),
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
    resume_core_abi_depth(guard.saved_depth);
    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    result
}

/// Allocates the Core-owned stack used for **policy execution**: prepared when a
/// policy endpoint is selected (outside policy execution), kept in the
/// scheduler's policy slot, and reused across policy calls.
///
/// A panic retains it (retired, never reused); a fresh explicit selection
/// prepares a fresh stack.
pub(crate) fn alloc_policy_stack() -> Option<MemoryLease> {
    memory::alloc_region(COMPONENT_STACK_BYTES).ok()
}

/// Calls the **selected scheduler policy** (`kcomp_service_dispatch`) on the
/// Core-owned stack prepared when the policy endpoint was selected, under
/// [`EscapeKind::PolicyCall`].
///
/// This is the execution boundary of the scheduling commit path
/// (`crate::sched::pick_next`).  It differs from [`call_component_service`]:
///
/// - **Core is the caller**: no caller principal and no caller task; the
///   boundary carries no task provenance (a policy panic must not inherit the
///   yielding task's identity);
/// - the stack is **prepared outside policy execution** (at selection), so the
///   hot path performs no allocation;
/// - a panic returns to the **suspended scheduler frame**, which still owns its
///   `IrqSaveGuard` — the boundary is escapable, and the stack is retained
///   (`mem::forget`, no allocator lock on the panic path) and retired by the
///   scheduler; it is never reused.
///
/// Returns the outcome plus the stack lease: `None` = the stack was retained
/// (panic); `Some` = the lease goes back to the policy slot for reuse.
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_component_policy(
    owner: ComponentId,
    endpoint: EndpointId,
    dispatcher: ServiceDispatch,
    instance_state: *mut (),
    port: u32,
    method: u32,
    frame: *const KcompCallFrame,
    stack: MemoryLease,
) -> (CallOutcome, Option<MemoryLease>) {
    #[cfg(test)]
    if let Some(simulated) = test_simulated_policy() {
        // host fake 后端不执行组件入口体：测试用模拟执行走**同一个** PolicyCall
        // 边界（门禁 / 记账与生产路径一致；真实栈切换由 QEMU 证明）。
        return match simulated {
            TestPolicySim::Dispatch(dispatch) => {
                let outcome = with_test_policy_boundary(owner, endpoint, || {
                    CallOutcome::Returned(dispatch(instance_state, port, method, frame))
                });
                (outcome, Some(stack))
            }
            TestPolicySim::Panicked => {
                // 与生产 panic 路径同一纪律：保留 lease（绝不复用），不碰分配器锁。
                core::mem::forget(stack);
                (CallOutcome::Panicked, None)
            }
        };
    }

    let stack_top = (stack.base() + stack.size()) & !(STACK_ALIGNMENT - 1);
    // 与 service call 相同的 irq-save 纪律：RISC-V context record 不携带
    // `sstatus.SIE`，Core 显式保存 / 恢复调用者的中断使能状态。
    let irq_flags = CpuImpl::disable_irq();
    let outcome = run_isolated_on(
        stack_top,
        IsolatedCall::Service {
            dispatcher,
            instance_state,
            port,
            method,
            frame,
        },
        EscapeKind::PolicyCall { owner, endpoint },
    );
    CpuImpl::restore_irq(irq_flags);

    match outcome {
        CallOutcome::Panicked => {
            // 保守驻留（与 service call 的 panic 路径同一纪律）：
            // 保留 lease、绝不复用这块栈，且 panic 路径上不碰分配器锁。
            core::mem::forget(stack);
            (CallOutcome::Panicked, None)
        }
        outcome => (outcome, Some(stack)),
    }
}

/// Shared body of the create / destroy boundaries: allocate a Core-owned stack,
/// install `kind` as the active escape guard, switch, and collect the outcome.
fn call_on_isolated_stack_with(call: IsolatedCall, kind: EscapeKind) -> CallOutcome {
    let IsolatedRun { outcome, stack } = run_isolated(call, kind);
    if let Some(stack) = stack {
        let _ = memory::free_region(stack);
    }
    outcome
}

/// One isolated-stack switch: the component entry outcome plus the Core-owned
/// stack lease, so the caller decides whether to reclaim it (create / destroy
/// always do; a panicked service call retains it).
struct IsolatedRun {
    outcome: CallOutcome,
    /// `None` = the stack could not be allocated; `outcome` is
    /// [`CallOutcome::NoStack`] and there is nothing to reclaim.
    stack: Option<MemoryLease>,
}

/// Shared mechanism of every isolated-stack boundary: allocate a Core-owned
/// stack, install `kind` as the active escape guard, switch, and collect the
/// outcome.
fn run_isolated(call: IsolatedCall, kind: EscapeKind) -> IsolatedRun {
    let stack = match memory::alloc_region(COMPONENT_STACK_BYTES) {
        Ok(stack) => stack,
        Err(_) => {
            return IsolatedRun {
                outcome: CallOutcome::NoStack,
                stack: None,
            };
        }
    };
    let stack_top = (stack.base() + stack.size()) & !(STACK_ALIGNMENT - 1);
    let outcome = run_isolated_on(stack_top, call, kind);
    IsolatedRun {
        outcome,
        stack: Some(stack),
    }
}

/// [`run_isolated`] on an **already allocated** Core-owned stack: install `kind`
/// as the active escape guard, switch, and collect the outcome.
///
/// `stack_top` must be the (aligned) one-past-end address of a live Core-owned
/// stack region.  The policy path uses this with the stack prepared at
/// selection; create / destroy / service calls allocate a fresh one per call.
fn run_isolated_on(stack_top: usize, call: IsolatedCall, kind: EscapeKind) -> CallOutcome {
    let mut core_context = CpuImpl::new_context(0, 0);
    let mut component_context = CpuImpl::new_context(trampoline as *const () as usize, stack_top);
    let mut guard = EscapeGuard {
        kind,
        from_context: &mut component_context,
        to_context: &mut core_context,
        call,
        returned: 0,
        state: GuardState::new(None),
        // From here on the stack carries component code: it must be escapable.
        saved_depth: suspend_core_abi_depth(),
    };
    guard.state = GuardState::new(replace_active(&mut guard));

    // The context records are local to this suspended caller frame and remain
    // valid until the component returns or `panic_escape` resumes this point.
    //
    // `tp` discipline is the same as `saved_depth` / `sstatus.SIE`: the context
    // record saves/restores it across the switch on both the normal-return and
    // panic-escape paths, so no extra register install is needed here.
    CpuImpl::context_switch(&mut core_context, &component_context);

    // Control is back on this Core frame (component return or panic escape):
    // restore the Core ABI depth the caller had before the boundary.
    resume_core_abi_depth(guard.saved_depth);

    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    match guard.state.panicked() {
        true => CallOutcome::Panicked,
        false => CallOutcome::Returned(guard.returned),
    }
}

/// Runs `f` inside an **IRQ attribution scope** ([`EscapeKind::Irq`]): Core
/// calls made by `f` resolve to `owner` with `task = None`.
///
/// Installed by [`crate::irq::on_irq`] around exactly one component
/// callback, using the same save/replace/restore discipline as the create and
/// destroy boundaries: the interrupted guard (task / init / exit / anchor) is
/// captured in the scope record and restored **explicitly** after `f` returns,
/// so nesting restores in order.  There is deliberately no `Drop` recovery —
/// the codebase has no unwinding (`panic = "abort"`); if a panic escapes the
/// scope, [`panic_escape`] restores the interrupted guard and refuses the
/// escape (fatal).
///
/// This is **bookkeeping for trusted KernelNative components, not an
/// authentication boundary**: it records who Core is dispatching for, it cannot
/// prove the callback code really belongs to `owner`.  The scope is synchronous
/// and non-yielding; scheduler-affecting Core operations are rejected while it
/// is active or anywhere beneath it ([`scheduling_forbidden`]).
pub(crate) fn with_irq_scope<R>(owner: ComponentId, f: impl FnOnce() -> R) -> R {
    let mut guard = EscapeGuard {
        kind: EscapeKind::Irq { owner },
        // Never used: an IRQ scope is not escapable (see `panic_escape`), and no
        // context switch may be attempted from the trap context.
        from_context: core::ptr::null_mut(),
        to_context: core::ptr::null_mut(),
        call: IsolatedCall::None,
        returned: 0,
        state: GuardState::new(None),
        // `f` is the component callback: it runs escapable (the IRQ guard itself
        // still refuses the escape; the depth is suspended so nested component
        // code reached beneath the scope is judged by its own boundary).
        saved_depth: suspend_core_abi_depth(),
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
    // Explicit restore (no unwinding; see module docs).  The record captured
    // the guard it replaced, so nested IRQ scopes pop in order.
    resume_core_abi_depth(guard.saved_depth);
    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    result
}

/// Scheduler hook: install the ambient guard before switching **into** a task.
///
/// The first transition out of the anchor saves the ambient (possibly init)
/// guard; later task-to-task switches only replace the task record.  Must be
/// called with no Core lock held and immediately before the context switch.
///
/// A task executes component code, so the Core ABI depth is zeroed here.  The
/// *outgoing* execution's depth is not stored in this record — the task guard is
/// overwritten on every switch; the scheduler frame that is suspended by the
/// switch (`sched::schedule_next`) saves its own depth and restores it when that
/// frame is resumed.
pub fn enter_task(task: TaskId, owner: ComponentId) {
    // SAFETY: [Category 2 — Data races] current CPU; called on the
    // synchronous scheduling path with all locks released.
    unsafe {
        if !(*local()).task_region {
            (*local()).anchor = (*local()).active;
            (*local()).task_region = true;
        }
        let guard = EscapeGuard {
            kind: EscapeKind::Task { task, owner },
            from_context: core::ptr::addr_of_mut!((*local()).scratch).cast::<ContextImpl>(),
            to_context: core::ptr::addr_of_mut!((*local()).abort).cast::<ContextImpl>(),
            call: IsolatedCall::None,
            returned: 0,
            state: GuardState::new(None),
            // Unused at the task boundary: see `EscapeGuard::saved_depth`.
            saved_depth: 0,
        };
        core::ptr::addr_of_mut!((*local()).task_guard).write(MaybeUninit::new(guard));
        (*local()).active = core::ptr::addr_of_mut!((*local()).task_guard).cast::<EscapeGuard>();
    }
    set_core_abi_depth(0);
}

/// Scheduler hook: restore the ambient guard before switching **into the anchor**.
pub fn enter_anchor() {
    // SAFETY: [Category 2 — Data races] current CPU; synchronous path.
    unsafe {
        (*local()).task_region = false;
        (*local()).active = (*local()).anchor;
        (*local()).anchor = core::ptr::null_mut();
    }
}

extern "C" fn trampoline() -> ! {
    let Some(guard_ptr) = active_guard() else {
        halt()
    };
    // Copy the invocation out with one short raw access.  No `&mut EscapeGuard`
    // may stay live across the component entry: the entry can reach the same
    // record through `panic_escape`'s raw pointer (or replace it), which would
    // invalidate a live `&mut` (Stacked Borrows).  All invocation metadata is
    // therefore copied into locals before the foreign call.
    // SAFETY: the active guard belongs to the caller frame suspended by
    // `run_isolated`; this context is the only execution using it.
    let call = unsafe { (*guard_ptr).call };
    let returned = match call {
        IsolatedCall::None => 0,
        IsolatedCall::Create {
            entry,
            args,
            out_state,
        } => {
            // SAFETY: `entry` comes from `LoadedComponent::create` (loader
            // validated + relocated the symbol); `args` / `out_state` point at
            // the suspended caller's still-live frame.
            let create: InstanceCreate = unsafe { core::mem::transmute(entry) };
            create(args, out_state)
        }
        IsolatedCall::Destroy { entry, state } => {
            // SAFETY: `entry` comes from the component's loaded image
            // (`ComponentRecord.loaded.destroy`; loader validated + relocated
            // the symbol); `state` is the component's opaque pointer (Core
            // never dereferences it).
            let destroy: InstanceDestroy = unsafe { core::mem::transmute(entry) };
            destroy(state)
        }
        IsolatedCall::Service {
            dispatcher,
            instance_state,
            port,
            method,
            frame,
        } => {
            // SAFETY: `dispatcher` comes from the component's loaded image
            // (`ComponentRecord.loaded.service_dispatch`; loader validated +
            // relocated the symbol); `instance_state` /
            // `frame` belong to the suspended caller (`call.rs` released every
            // lock before entering the boundary, and the frame stays valid for
            // the duration of the call).
            let dispatch: ServiceDispatch = dispatcher;
            dispatch(instance_state, port, method, frame)
        }
    };
    // Short raw write: this context's own record (a panic never returns here —
    // `panic_escape` switches to the Core context instead).
    // SAFETY: [Category 2 — Data races] same record as above; the entry has
    // returned, so nothing else can mutate it until `switch_to_core`.
    unsafe { (*guard_ptr).returned = returned };
    switch_to_core(guard_ptr)
}

/// Runs on the dedicated abort stack after a task panic; never returns to the
/// panicking frame.  Commits the dead task and fails its component.
///
/// The abort bookkeeping is **Core code**: it is wrapped Core-critical so a
/// panic inside it (`fail_component` → registry / trace / resource locks,
/// rescheduling) cannot attempt a second escape through the dead task's guard
/// and re-enter this very context.
extern "C" fn task_abort_trampoline() -> ! {
    let Some(guard_ptr) = active_guard() else {
        halt()
    };
    // SAFETY: [Category 2 — Data races] the record was installed by the
    // scheduler on this CPU and is not mutated while this context runs.
    let EscapeKind::Task { task, owner } = (unsafe { (*guard_ptr).kind }) else {
        // An abort context was entered without a task record: a Core invariant
        // break.  Halting is safer than resuming an unknown context.
        halt()
    };
    with_core_critical(|| crate::sched::abort_current_task(task, owner))
}

/// Transfers control from the escaping execution to the Core-owned context
/// recorded in the guard: the saved caller for init / exit / service call, the
/// task-abort context for a task.  Never returns to the escaping frame.
///
/// # `tp`
///
/// The switch restores `tp` from `to_context` like every other register in the
/// record: the caller's value for init / exit / service / policy (captured when
/// the boundary was entered), and `0` — Core — for the task-abort context.  The
/// escaping execution's `tp` is saved into `from_context` and never leaks into
/// the resumed Core frame; no extra install is needed on this path.
///
/// Takes a raw pointer (not `&mut`): the escaping execution may have reached the
/// record through a raw pointer of its own, so no live reference may exist here.
fn switch_to_core(guard_ptr: *mut EscapeGuard) -> ! {
    // SAFETY: both context records live in the suspended caller frame of
    // `run_isolated` (or are the scheduler's static abort records); the raw
    // reads copy the two pointers before any switch.
    unsafe {
        let from_context = (*guard_ptr).from_context;
        let to_context = (*guard_ptr).to_context;
        CpuImpl::context_switch(&mut *from_context, &*to_context);
    }
    halt()
}

/// Decides whether the current execution may escape a panic.  `Some(guard)` =
/// eligible; `None` = refused.  Kept separate from [`panic_escape`] so host tests
/// can pin the decision: the fake context backend's `context_switch` is a no-op
/// followed by `halt`, so a real escape can only be proven on QEMU/ArchTest.
///
/// Refusals, in order:
/// - no active boundary: the panic is outside any component boundary;
/// - non-escapable boundary (an IRQ attribution scope, [`with_irq_scope`]): no
///   Core-owned context to resume — the interrupted guard is restored **here**
///   (explicit recovery; never rely on `Drop`, there is no unwinding), so a stale
///   IRQ scope cannot outlive the attempt;
/// - **Core ABI depth > 0** ([`with_core_critical`]): Core code is on the stack,
///   i.e. this is a **Core** panic, not a component panic.  Escaping would
///   misattribute a Core bug to the boundary owner and resume the recovery path
///   (`fail_component` → registry / trace / resource locks) while the panicking
///   Core frame still holds its locks → deadlock.  The escapable guard is left
///   untouched: the panic stays fatal.
fn escape_target() -> Option<*mut EscapeGuard> {
    let guard_ptr = active_guard()?;
    // SAFETY: `guard_ptr` is installed by the suspended caller/scheduler on this
    // CPU and remains valid until it is replaced after the switch back.
    let kind = unsafe { (*guard_ptr).kind };
    if !kind.is_escapable() {
        // An IRQ attribution scope has no Core-owned context to resume, and
        // escaping into the interrupted task would misattribute the callback's
        // panic to a task that did not panic.
        // SAFETY: [Category 2 — Data races] the record is live and this is the
        // only execution touching it; `previous` is a plain pointer copy.
        let previous = unsafe { (*guard_ptr).state.previous() };
        let _ = replace_active(previous.unwrap_or(core::ptr::null_mut()));
        return None;
    }
    if core_abi_depth() > 0 {
        return None;
    }
    // 没有可恢复的 Core 上下文（跨 AS service 边界：trap 恢复由跨 AS 现场负责，
    // 拒绝逃逸，保持 fatal——绝不切到空上下文。
    if unsafe { (*guard_ptr).from_context.is_null() } {
        // SAFETY: [Category 2 — Data races] the record is live and this is the
        // only execution touching it; `previous` is a plain pointer copy.
        let previous = unsafe { (*guard_ptr).state.previous() };
        let _ = replace_active(previous.unwrap_or(core::ptr::null_mut()));
        return None;
    }
    Some(guard_ptr)
}

/// Escapes an active component panic without allocation, logging, or locking.
///
/// Every escapable **component** boundary (init / exit / task / service call)
/// escapes through `from_context` → `to_context`; the resumed Core context
/// performs the containment bookkeeping.  Returns `false` when the panic
/// originated outside an isolated component, inside an IRQ attribution scope
/// ([`with_irq_scope`]), or while Core ABI code is on the stack (see
/// [`escape_target`]) — all three stay fatal.  A `true` result is unreachable in
/// a functioning context backend because the switch resumes the Core context
/// instead of this panic handler.
pub fn panic_escape() -> bool {
    // 跨 AS（Isolated）执行：恢复出口是 trampoline 的 Core 延续（`isolated::enter`
    // 会返回 `Outcome::Faulted`，由调用方边界做清理）。组件在 Isolated AS 里
    // panic 就是组件失败；生命周期 / service guard 提供身份，恢复由跨 AS 现场负责。
    // Core-critical 深度 > 0 = Core 代码在栈上：Core panic，保持 fatal。
    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64", test))]
    if let Some(cross) = cross_as::active_cross_as() {
        if core_abi_depth() > 0 {
            return false;
        }
        mark_active_panicked();
        // SAFETY: 现场仍挂起；`abandon` 永不返回本执行流。
        unsafe { ((*cross).abandon)((*cross).context) }
    }

    let Some(guard_ptr) = escape_target() else {
        return false;
    };
    // SAFETY: [Category 2 — Data races] short raw access; the record is live and
    // `switch_to_core` never returns to this frame.
    unsafe { (*guard_ptr).state.mark_panicked() };
    switch_to_core(guard_ptr)
}

/// 标记当前最内层 guard 为 panicked（若存在）。跨 AS 的 panic 恢复不经过
/// [`escape_target`]（它会在 null-context guard 上弹链），因此单独提供。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", test))]
fn mark_active_panicked() {
    if let Some(guard_ptr) = active_guard() {
        // SAFETY: [Category 2 — Data races] CPU-local; the record is live
        // and this is the only execution touching it.
        unsafe { (*guard_ptr).state.mark_panicked() };
    }
}

/// Renders one short panic diagnostic to a direct (lock-free) writer:
///
/// ```text
/// [panic] component=<id>[ task=<id>] at <file:line>: <message>
/// ```
///
/// `location`/`message` come from the boot panic handler's `PanicInfo`; the
/// message is printed verbatim (no allocation, no sanitization).
pub fn write_escape_line(
    writer: &mut dyn core::fmt::Write,
    escape: EscapeInfo,
    location: Option<&core::panic::Location<'_>>,
    message: Option<&dyn core::fmt::Display>,
) -> core::fmt::Result {
    writer.write_str("\n[panic] component=")?;
    match escape.owner() {
        Some(owner) => write!(writer, "{}", owner.raw())?,
        None => writer.write_str("?")?,
    }
    if let Some(task) = escape.task() {
        write!(writer, " task={}", task.raw())?;
    }
    writer.write_str(" at ")?;
    match location {
        Some(location) => write!(writer, "{}:{}", location.file(), location.line())?,
        None => writer.write_str("?")?,
    }
    writer.write_str(": ")?;
    match message {
        Some(message) => write!(writer, "{}", message)?,
        None => writer.write_str("<no message>")?,
    }
    writer.write_str("\n")
}

fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

// —— Test-only boundary scaffolding ——
//
// These helpers let host tests exercise the process-global boundary stack
// without a context switch, and serialize the tests that mutate it.

/// Serializes tests that mutate the process-global active boundary.
///
/// rank = BOUNDARY（规范顺序 `SCHED → LOAD → IRQ → TIMER → BOUNDARY → MACHINE → MEMORY → TRACE`；见
/// [`crate::test_support`]）。
#[cfg(test)]
static TEST_BOUNDARY_LOCK: crate::test_support::TestLock =
    crate::test_support::TestLock::new(crate::test_support::Rank::Boundary);

/// Acquires the test-only active-boundary lock.
#[cfg(test)]
pub(crate) fn test_boundary_lock() -> crate::test_support::TestLockGuard<'static> {
    TEST_BOUNDARY_LOCK.lock()
}

/// Runs `f` with an init escape boundary installed over the current one,
/// without a context switch.  Mirrors the guard push/pop in
/// [`call_on_isolated_stack_with`], so host tests can exercise the boundary nesting
/// that principal resolution depends on.
#[cfg(test)]
pub(crate) fn with_test_init_boundary<R>(owner: Option<ComponentId>, f: impl FnOnce() -> R) -> R {
    let mut from_context = CpuImpl::new_context(0, 0);
    let mut to_context = CpuImpl::new_context(0, 0);
    let mut guard = EscapeGuard {
        kind: EscapeKind::Init { owner },
        from_context: &mut from_context,
        to_context: &mut to_context,
        call: IsolatedCall::None,
        returned: 0,
        state: GuardState::new(None),
        // Mirrors `run_isolated`: component code runs with the depth suspended.
        saved_depth: suspend_core_abi_depth(),
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
    resume_core_abi_depth(guard.saved_depth);
    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    result
}

/// Runs `f` with an exit escape boundary installed over the current one, without
/// a context switch.  Mirrors the guard installed by [`call_component_destroy`], so
/// host tests can assert the ambient identity of the stopped instance (the fake
/// context backend does not actually execute component entries).
#[cfg(test)]
pub(crate) fn with_test_exit_boundary<R>(owner: ComponentId, f: impl FnOnce() -> R) -> R {
    let mut from_context = CpuImpl::new_context(0, 0);
    let mut to_context = CpuImpl::new_context(0, 0);
    let mut guard = EscapeGuard {
        kind: EscapeKind::Exit { owner },
        from_context: &mut from_context,
        to_context: &mut to_context,
        call: IsolatedCall::None,
        returned: 0,
        state: GuardState::new(None),
        // Mirrors `run_isolated`: component code runs with the depth suspended.
        saved_depth: suspend_core_abi_depth(),
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
    resume_core_abi_depth(guard.saved_depth);
    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    result
}

/// Runs `f` with a **service-call** escape boundary installed over the current
/// one, without a context switch.  Mirrors the guard installed by
/// [`call_component_service`], so host tests can exercise the boundary nesting,
/// principal resolution, re-entry, and ancestor gates that the fake context
/// backend cannot reach (it does not execute component entries).
#[cfg(test)]
pub(crate) fn with_test_service_boundary<R>(
    owner: ComponentId,
    endpoint: EndpointId,
    caller_task: Option<TaskId>,
    f: impl FnOnce() -> R,
) -> R {
    let mut from_context = CpuImpl::new_context(0, 0);
    let mut to_context = CpuImpl::new_context(0, 0);
    let mut guard = EscapeGuard {
        kind: EscapeKind::ServiceCall {
            owner,
            endpoint,
            caller_task,
        },
        from_context: &mut from_context,
        to_context: &mut to_context,
        call: IsolatedCall::None,
        returned: 0,
        state: GuardState::new(None),
        // Mirrors `run_isolated`: the provider dispatcher runs escapable.
        saved_depth: suspend_core_abi_depth(),
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
    resume_core_abi_depth(guard.saved_depth);
    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    result
}

/// Runs `f` with a **policy-call** escape boundary installed over the current
/// one, without a context switch.  Mirrors the guard installed by
/// [`call_component_policy`], so host tests can exercise the policy-context
/// gates (generic endpoint calls / nested creation / policy replacement) and the
/// nesting behavior that the fake context backend cannot reach.
#[cfg(test)]
pub(crate) fn with_test_policy_boundary<R>(
    owner: ComponentId,
    endpoint: EndpointId,
    f: impl FnOnce() -> R,
) -> R {
    let mut from_context = CpuImpl::new_context(0, 0);
    let mut to_context = CpuImpl::new_context(0, 0);
    let mut guard = EscapeGuard {
        kind: EscapeKind::PolicyCall { owner, endpoint },
        from_context: &mut from_context,
        to_context: &mut to_context,
        call: IsolatedCall::None,
        returned: 0,
        state: GuardState::new(None),
        // Mirrors `call_component_policy`: the policy dispatcher runs escapable.
        saved_depth: suspend_core_abi_depth(),
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
    resume_core_abi_depth(guard.saved_depth);
    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    result
}

/// Test-only **simulated policy execution** (thread-local; host fake context
/// backend does not execute component entry bodies).
///
/// When installed, [`call_component_policy`] does not perform a real stack
/// switch: [`TestPolicySim::Dispatch`] runs the simulated dispatcher
/// synchronously inside the same [`EscapeKind::PolicyCall`] boundary (gates and
/// bookkeeping stay identical), [`TestPolicySim::Panicked`] reports the panic
/// outcome with the stack retained / retired.  The production path is
/// untouched: real policy execution and stack switching are proven on QEMU.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum TestPolicySim {
    /// Run this dispatcher inside the PolicyCall boundary.
    Dispatch(ServiceDispatch),
    /// Report `CallOutcome::Panicked` (stack retained and retired).
    Panicked,
}

#[cfg(test)]
std::thread_local! {
    static TEST_POLICY_SIM: core::cell::Cell<Option<TestPolicySim>> =
        const { core::cell::Cell::new(None) };
}

/// The installed simulated policy execution, if any (see [`TestPolicySim`]).
#[cfg(test)]
fn test_simulated_policy() -> Option<TestPolicySim> {
    TEST_POLICY_SIM.with(core::cell::Cell::get)
}

/// Installs a simulated policy execution for the duration of `f`.
#[cfg(test)]
fn with_test_policy_simulation<R>(sim: TestPolicySim, f: impl FnOnce() -> R) -> R {
    TEST_POLICY_SIM.with(|slot| {
        let previous = slot.replace(Some(sim));
        let result = f();
        slot.set(previous);
        result
    })
}

/// Runs `f` with `dispatch` installed as the simulated policy dispatcher
/// (test-only; see [`TestPolicySim`]).
#[cfg(test)]
pub(crate) fn with_test_policy_dispatch<R>(dispatch: ServiceDispatch, f: impl FnOnce() -> R) -> R {
    with_test_policy_simulation(TestPolicySim::Dispatch(dispatch), f)
}

/// Runs `f` with a simulated policy **panic** installed (test-only; the stack is
/// retained / retired exactly like the production panic path).
#[cfg(test)]
pub(crate) fn with_test_policy_panic<R>(f: impl FnOnce() -> R) -> R {
    with_test_policy_simulation(TestPolicySim::Panicked, f)
}

/// Marks the active escape as panicked, as the boot panic handler does before
/// escaping.  Lets host tests assert that popping a panicked guard still
/// restores the previous boundary.
#[cfg(test)]
pub(crate) fn test_mark_active_panicked() {
    mark_active_panicked();
}

/// Whether the active escape record is marked panicked (test-only probe; pairs
/// with [`test_mark_active_panicked`]).
#[cfg(test)]
pub(crate) fn test_active_panicked() -> bool {
    active_guard().is_some_and(|guard_ptr| {
        // SAFETY: [Category 2 — Data races] current CPU / test boundary
        // lock; the record is live and only this test reads it.
        unsafe { (*guard_ptr).state.panicked() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn execution_identity_is_cpu_local() {
        let _boundary = test_boundary_lock();
        let original = CpuImpl::current_cpu().unwrap();
        let base = CpuImpl::per_cpu_base().unwrap_or(core::ptr::NonNull::dangling());
        // A second CPU must not inherit CPU0's task or create attribution.
        unsafe { CpuImpl::install_per_cpu_base(crate::machine::CpuId(0), base) };
        let previous = replace_creating(Some(ComponentId::from_raw(70)));
        enter_task(TaskId::from_raw(80), ComponentId::from_raw(70));
        unsafe { CpuImpl::install_per_cpu_base(crate::machine::CpuId(1), base) };
        assert_eq!(creating(), None);
        assert_eq!(active_escape(), None);
        enter_task(TaskId::from_raw(81), ComponentId::from_raw(71));
        replace_creating(Some(ComponentId::from_raw(71)));
        enter_anchor();
        replace_creating(None);
        unsafe { CpuImpl::install_per_cpu_base(crate::machine::CpuId(0), base) };
        assert_eq!(creating(), Some(ComponentId::from_raw(70)));
        assert_eq!(
            active_escape().unwrap().owner(),
            Some(ComponentId::from_raw(70))
        );
        enter_anchor();
        replace_creating(previous);
        unsafe { CpuImpl::install_per_cpu_base(original, base) };
    }

    #[test]
    fn empty_create_args_have_no_config_payload() {
        // 默认配置 = 无负载：config_abi = 0，config = NULL，config_len = 0。
        let args = KcompCreateArgs::empty();
        assert_eq!(args.config_abi, 0);
        assert!(args.config.is_null());
        assert_eq!(args.config_len, 0);
    }

    #[test]
    fn kcomp_abi_is_the_manual_anchor() {
        // 当前 Core import / 生命周期 exact 指纹；来源为 abi/component.toml。
        assert_eq!(KCOMP_ABI, 0x9D73_405B_B2F8_16C0);
    }

    #[test]
    fn guard_state_restores_previous_target_after_nested_panic() {
        // Given: an outer escape target and a nested guard.
        let mut active = Some(1usize);
        let mut nested = GuardState::new(active);

        // When: the nested component panics and its guard is popped.
        active = Some(2);
        assert_eq!(active, Some(2));
        nested.mark_panicked();
        active = nested.previous();

        // Then: the nested panic is recorded and the outer target remains available.
        assert!(nested.panicked());
        assert_eq!(active, Some(1));
    }

    #[test]
    fn escape_info_reports_init_owner_without_task() {
        // Given: an init-boundary escape for component 4.
        let info = EscapeInfo {
            kind: EscapeKind::Init {
                owner: Some(ComponentId::from_raw(4)),
            },
        };

        // When / Then: it names the component and has no task.
        assert_eq!(info.owner(), Some(ComponentId::from_raw(4)));
        assert_eq!(info.task(), None);
    }

    #[test]
    fn escape_info_reports_task_and_owner() {
        // Given: a task-boundary escape.
        let info = EscapeInfo {
            kind: EscapeKind::Task {
                task: TaskId::from_raw(7),
                owner: ComponentId::from_raw(3),
            },
        };

        // When / Then: both identities are visible to the diagnostics.
        assert_eq!(info.owner(), Some(ComponentId::from_raw(3)));
        assert_eq!(info.task(), Some(TaskId::from_raw(7)));
    }

    #[test]
    fn escape_info_reports_exit_owner_without_task() {
        // Given: an exit-boundary escape for the stopped instance 9.
        let info = EscapeInfo {
            kind: EscapeKind::Exit {
                owner: ComponentId::from_raw(9),
            },
        };

        // When / Then: it names the instance and has no task.
        assert_eq!(info.owner(), Some(ComponentId::from_raw(9)));
        assert_eq!(info.task(), None);
    }

    #[test]
    fn escape_info_reports_irq_owner_without_task() {
        // Given: an IRQ attribution scope for the line owner 7.
        let info = EscapeInfo {
            kind: EscapeKind::Irq {
                owner: ComponentId::from_raw(7),
            },
        };

        // When / Then: it names the line owner and carries no task.
        assert_eq!(info.owner(), Some(ComponentId::from_raw(7)));
        assert_eq!(info.task(), None);
    }

    /// 验收：IRQ scope 报告 line owner / 无 task；离开后**被中断的 task 边界**
    /// 原样恢复，且上下文种类不再是 IRQ。
    #[test]
    fn irq_scope_installs_owner_and_restores_interrupted_boundary() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));
        assert!(!in_irq_context());

        let owner = ComponentId::from_raw(0xBEEF);
        with_irq_scope(owner, || {
            assert!(in_irq_context(), "IRQ scope is the active context kind");
            let info = active_escape().expect("IRQ scope is an active boundary");
            assert_eq!(info.owner(), Some(owner));
            assert_eq!(info.task(), None, "an IRQ callback is not a task");
        });

        assert!(!in_irq_context());
        let info = active_escape().expect("interrupted task boundary restored");
        assert_eq!(info.owner(), Some(ComponentId::from_raw(3)));
        assert_eq!(info.task(), Some(TaskId::from_raw(7)));
        enter_anchor();
    }

    /// 验收：嵌套 IRQ scope 按后进先出恢复（内层退出 → 外层，外层退出 → task）。
    #[test]
    fn nested_irq_scopes_restore_in_order() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        let outer = ComponentId::from_raw(9);
        let inner = ComponentId::from_raw(11);
        with_irq_scope(outer, || {
            assert_eq!(active_escape().unwrap().owner(), Some(outer));
            with_irq_scope(inner, || {
                assert_eq!(active_escape().unwrap().owner(), Some(inner));
            });
            assert_eq!(
                active_escape().unwrap().owner(),
                Some(outer),
                "inner scope restores the outer IRQ scope"
            );
        });

        let info = active_escape().expect("task boundary restored");
        assert_eq!(info.owner(), Some(ComponentId::from_raw(3)));
        assert_eq!(info.task(), Some(TaskId::from_raw(7)));
        enter_anchor();
    }

    /// 验收：IRQ scope 内 panic escape 被拒绝（返回 `false`，panic 在 IRQ
    /// 上下文致命），且**当场**恢复被中断的 guard —— 不会留下 stale IRQ scope，
    /// 也不会把回调 panic 误算到被中断的 task 头上。
    #[test]
    fn panic_escape_inside_irq_scope_restores_and_refuses() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        with_irq_scope(ComponentId::from_raw(0xBEEF), || {
            assert!(
                !panic_escape(),
                "IRQ scope has no Core context to resume — escape refused"
            );
            assert!(!in_irq_context(), "stale IRQ scope must not survive");
            let info = active_escape().expect("interrupted guard restored by panic_escape");
            assert_eq!(info.owner(), Some(ComponentId::from_raw(3)));
            assert_eq!(info.task(), Some(TaskId::from_raw(7)));
        });

        // The scope's own explicit restore at return is idempotent.
        let info = active_escape().expect("task boundary still restored");
        assert_eq!(info.task(), Some(TaskId::from_raw(7)));
        enter_anchor();
    }

    // ------------------------------------------------------------------
    // Core ABI depth（`with_core_critical`）：只有组件代码可逃逸
    // ------------------------------------------------------------------

    /// 验收：Core ABI 深度 > 0（导出体内）时 `panic_escape()` 拒绝，且**不弹
    /// 边界、不标记 panicked** —— Core panic 保持致命，恢复路径不会带着 Core 锁
    /// 进入 `fail_component`（死锁源）。
    #[test]
    fn panic_escape_refuses_inside_core_critical_and_keeps_boundary() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));
        assert_eq!(core_abi_depth(), 0, "a task runs component code");

        with_core_critical(|| {
            assert_eq!(core_abi_depth(), 1, "an export body is Core-critical");
            assert!(
                !panic_escape(),
                "a panic inside Core ABI code must stay fatal"
            );
            let info = active_escape().expect("boundary left intact by the refusal");
            assert_eq!(info.owner(), Some(ComponentId::from_raw(3)));
            assert_eq!(info.task(), Some(TaskId::from_raw(7)));
            assert!(
                !test_active_panicked(),
                "the refusal must not mark the boundary as a component panic"
            );
        });

        assert_eq!(core_abi_depth(), 0, "the critical scope balances its depth");
        enter_anchor();
    }

    /// 验收（回归）：无 critical scope 时组件代码照常可逃逸；无边界时
    /// `panic_escape()` 仍返回 `false`。真实的上下文切换由 QEMU/ArchTest 证明
    /// （host fake 后端的 `context_switch` 是 no-op + halt，不会真的切走）。
    #[test]
    fn panic_escape_is_eligible_outside_core_critical() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        assert!(!panic_escape(), "no boundary → refusal (existing behavior)");

        enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));
        assert_eq!(core_abi_depth(), 0);
        assert!(
            escape_target().is_some(),
            "component code without a critical scope may escape"
        );
        enter_anchor();
    }

    /// 验收：critical scope 内**嵌套的组件边界**仍然可逃逸（边界安装把深度挂起
    /// 到 0），边界弹出后恢复 critical scope 的深度。
    #[test]
    fn nested_component_boundary_inside_core_critical_is_escapable() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        with_core_critical(|| {
            assert_eq!(core_abi_depth(), 1);
            with_test_init_boundary(Some(ComponentId::from_raw(4)), || {
                assert_eq!(core_abi_depth(), 0, "the boundary suspends the Core depth");
                assert!(
                    escape_target().is_some(),
                    "nested component code stays escapable inside a critical scope"
                );
            });
            assert_eq!(
                core_abi_depth(),
                1,
                "the nested boundary restores the depth"
            );
        });
        assert_eq!(core_abi_depth(), 0);

        // Outside the critical scope the task boundary is still active and
        // remains escapable; leaving it returns to the no-boundary refusal.
        assert_eq!(active_escape().unwrap().task(), Some(TaskId::from_raw(7)));
        assert!(escape_target().is_some());
        enter_anchor();
        assert!(!panic_escape(), "no boundary → refusal again");
    }

    /// 验收：深度在嵌套边界**正常返回**与**panic 之后**都正确恢复（边界弹出
    /// 不依赖 Drop / 展开：这里显式标记 panicked，再由 guard 弹出恢复）。
    #[test]
    fn boundary_depth_is_restored_after_return_and_after_panic() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        with_core_critical(|| {
            let provider = ComponentId::from_raw(9);
            with_test_service_boundary(provider, EndpointId::from_raw(1), None, || {
                assert_eq!(core_abi_depth(), 0);
            });
            assert_eq!(core_abi_depth(), 1, "a normal return restores the depth");

            with_test_service_boundary(provider, EndpointId::from_raw(2), None, || {
                assert_eq!(core_abi_depth(), 0);
                test_mark_active_panicked();
                assert!(test_active_panicked());
            });
            assert_eq!(
                core_abi_depth(),
                1,
                "a panicked boundary still restores the depth"
            );
        });
        assert_eq!(core_abi_depth(), 0);
        enter_anchor();
    }

    /// 验收：IRQ scope 也把深度挂起（回调是组件代码）并在返回时恢复。
    #[test]
    fn irq_scope_suspends_and_restores_core_abi_depth() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        with_core_critical(|| {
            with_irq_scope(ComponentId::from_raw(0xBEEF), || {
                assert_eq!(core_abi_depth(), 0, "an IRQ callback is component code");
            });
            assert_eq!(core_abi_depth(), 1);
        });
        assert_eq!(core_abi_depth(), 0);
        enter_anchor();
    }

    /// 验收：create / destroy / service 共用的生产边界（`run_isolated`）在安装时
    /// 挂起深度、Core 帧恢复时还原（host fake 后端不执行组件入口，只验证边界
    /// 记账；真实切换由 QEMU 证明）。
    #[test]
    fn isolated_stack_boundary_suspends_and_restores_core_abi_depth() {
        let _boundary = test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        enter_anchor();
        assert_eq!(core_abi_depth(), 0);

        with_core_critical(|| {
            let run = run_isolated(IsolatedCall::None, EscapeKind::Init { owner: None });
            assert_eq!(run.outcome, CallOutcome::Returned(0));
            assert_eq!(
                core_abi_depth(),
                1,
                "the isolated boundary restores the Core frame's depth"
            );
            if let Some(stack) = run.stack {
                let _ = crate::memory::free_region(stack);
            }
        });
        assert_eq!(core_abi_depth(), 0);
        enter_anchor();
    }

    #[test]
    fn exit_boundary_is_the_ambient_identity_and_not_a_publish_principal() {
        // Given: no ambient boundary.
        let _boundary = test_boundary_lock();
        enter_anchor();
        assert!(active_escape().is_none());

        // When: an exit boundary for component 5 is active (as during kcomp_instance_destroy).
        let owner = ComponentId::from_raw(5);
        with_test_exit_boundary(owner, || {
            // Then: Core calls are attributed to the stopped instance...
            let ambient = crate::resource::RequestContext::ambient().expect("ambient identity");
            assert_eq!(ambient.component, owner);
            assert_eq!(ambient.task, None);
            // ...and publish remains an init-time operation.
            assert!(crate::resource::RequestContext::ambient_init().is_none());
        });

        // Then: the previous boundary is restored after the hook returns.
        assert!(active_escape().is_none());
    }

    #[test]
    fn escape_line_formats_task_boundary() {
        // Given: a task escape and a direct string writer.
        let mut out = String::new();

        // When: the panic handler renders its one-line diagnostic.
        write_escape_line(
            &mut out,
            EscapeInfo {
                kind: EscapeKind::Task {
                    task: TaskId::from_raw(2),
                    owner: ComponentId::from_raw(5),
                },
            },
            None,
            Some(&format_args!("boom")),
        )
        .unwrap();

        // Then: the line carries component, task, location, and message.
        assert_eq!(out, "\n[panic] component=5 task=2 at ?: boom\n");
    }

    #[test]
    fn task_region_switches_replace_and_anchor_restores() {
        // Given: no ambient escape (anchor context); serialize boundary mutation.
        let _boundary = test_boundary_lock();
        enter_anchor();
        assert!(active_escape().is_none());

        // When: the scheduler switches into a component task.
        enter_task(TaskId::from_raw(9), ComponentId::from_raw(2));

        // Then: the ambient guard reports that task.
        let info = active_escape().expect("task escape active");
        assert_eq!(info.task(), Some(TaskId::from_raw(9)));
        assert_eq!(info.owner(), Some(ComponentId::from_raw(2)));

        // When: a task-to-task switch happens, only the record is replaced.
        enter_task(TaskId::from_raw(10), ComponentId::from_raw(2));
        assert_eq!(active_escape().unwrap().task(), Some(TaskId::from_raw(10)));

        // When: control returns to the anchor.
        enter_anchor();

        // Then: the ambient guard is cleared again.
        assert!(active_escape().is_none());
    }

    #[test]
    fn escape_line_omits_task_at_init_boundary() {
        // Given: an anonymous init escape.
        let mut out = String::new();

        // When: rendered without a known owner or message.
        write_escape_line(
            &mut out,
            EscapeInfo {
                kind: EscapeKind::Init { owner: None },
            },
            None,
            None,
        )
        .unwrap();

        // Then: the task field is omitted and unknown fields are marked `?`.
        assert_eq!(out, "\n[panic] component=? at ?: <no message>\n");
    }

    // ------------------------------------------------------------------
    // Service-call boundary (`kcore_endpoint_call`)
    // ------------------------------------------------------------------

    #[test]
    fn escape_info_reports_service_call_owner_and_caller_task() {
        // Given: a service-call escape from task 7 into provider 4.
        let info = EscapeInfo {
            kind: EscapeKind::ServiceCall {
                owner: ComponentId::from_raw(4),
                endpoint: EndpointId::from_raw(9),
                caller_task: Some(TaskId::from_raw(7)),
            },
        };

        // When / Then: the provider is the principal; the caller task is
        // provenance, not authority over that task.
        assert_eq!(info.owner(), Some(ComponentId::from_raw(4)));
        assert_eq!(info.task(), Some(TaskId::from_raw(7)));

        // A call from a non-task boundary (init / exit) carries no task.
        let no_task = EscapeInfo {
            kind: EscapeKind::ServiceCall {
                owner: ComponentId::from_raw(4),
                endpoint: EndpointId::from_raw(9),
                caller_task: None,
            },
        };
        assert_eq!(no_task.owner(), Some(ComponentId::from_raw(4)));
        assert_eq!(no_task.task(), None);
    }

    /// 验收：service call 与 init / exit / task 一样可逃逸；IRQ scope 保持致命。
    /// 真实的上下文切换（真机）由 QEMU 证明——host fake 后端不执行组件入口。
    #[test]
    fn service_call_boundary_is_escapable_unlike_irq() {
        assert!(EscapeKind::Init { owner: None }.is_escapable());
        assert!(
            EscapeKind::Exit {
                owner: ComponentId::from_raw(1)
            }
            .is_escapable()
        );
        assert!(
            EscapeKind::Task {
                task: TaskId::from_raw(1),
                owner: ComponentId::from_raw(1),
            }
            .is_escapable()
        );
        assert!(
            EscapeKind::ServiceCall {
                owner: ComponentId::from_raw(1),
                endpoint: EndpointId::from_raw(1),
                caller_task: None,
            }
            .is_escapable(),
            "a provider panic must escape to the Core-owned caller frame"
        );
        assert!(
            !EscapeKind::Irq {
                owner: ComponentId::from_raw(1)
            }
            .is_escapable(),
            "an IRQ callback has no Core-owned context to resume — stays fatal"
        );
    }

    /// 验收：service 祖先（即使上面盖着嵌套的 init 边界）禁止调度；
    /// IRQ scope 藏在生命周期边界下面也能被祖先遍历找到。
    #[test]
    fn service_ancestor_forbids_scheduling_beneath_nested_lifecycle_boundaries() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        let provider = ComponentId::from_raw(0xB);
        let endpoint = EndpointId::from_raw(3);
        assert!(!scheduling_forbidden());

        with_test_service_boundary(provider, endpoint, None, || {
            assert!(scheduling_forbidden(), "a service call forbids scheduling");
            assert!(!irq_in_chain());

            with_test_init_boundary(Some(ComponentId::from_raw(0xC)), || {
                assert!(
                    scheduling_forbidden(),
                    "a nested init boundary must not re-open the scheduler"
                );
                assert!(!in_irq_context(), "top guard is the init boundary");

                with_irq_scope(ComponentId::from_raw(0xD), || {
                    assert!(scheduling_forbidden());
                    assert!(
                        irq_in_chain(),
                        "an IRQ scope beneath lifecycle boundaries is still found"
                    );
                    assert!(in_irq_context(), "innermost guard is the IRQ scope");
                });
            });

            assert!(scheduling_forbidden(), "service ancestor restored");
        });

        assert!(!scheduling_forbidden());
        assert!(!irq_in_chain());
        enter_anchor();
    }

    /// 验收：跨 AS（Isolated provider）service 边界与同域 service 边界同一身份
    /// 纪律（principal = provider、caller task = provenance、调度禁止、re-entry
    /// 可见），但**不可逃逸**——guard 没有可恢复的 Core 上下文（切换由 跨 AS trampoline
    /// 负责），`escape_target` 显式拒绝，panic 保持 fatal。
    #[test]
    fn isolated_service_boundary_has_no_escapable_core_context() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));
        let provider = ComponentId::from_raw(0x1A);
        let endpoint = EndpointId::from_raw(5);

        with_isolated_service_boundary(provider, endpoint, Some(TaskId::from_raw(7)), || {
            let info = active_escape().expect("isolated service boundary installed");
            assert_eq!(info.owner(), Some(provider), "principal = provider");
            assert_eq!(info.task(), Some(TaskId::from_raw(7)), "provenance only");
            assert!(scheduling_forbidden(), "a service call forbids scheduling");
            assert!(
                provider_in_active_chain(provider),
                "re-entry detection sees the running provider"
            );
            assert!(
                escape_target().is_none(),
                "no Core-owned context to resume: the escape must be refused"
            );
        });

        // 边界恢复：caller 的 task 边界原样存活（拒绝逃逸不破坏链）。
        let info = active_escape().expect("caller boundary restored");
        assert_eq!(info.owner(), Some(ComponentId::from_raw(3)));
        assert_eq!(info.task(), Some(TaskId::from_raw(7)));
        assert!(!scheduling_forbidden());
        assert!(!provider_in_active_chain(provider));
        enter_anchor();
    }

    #[test]
    fn isolated_lifecycle_fault_preserves_caller_identity_and_core_depth() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        let caller = ComponentId::from_raw(3);
        let callee = ComponentId::from_raw(0x1A);
        let task = TaskId::from_raw(7);
        enter_task(task, caller);

        with_core_critical(|| {
            with_isolated_create_boundary(callee, || {
                assert_eq!(core_abi_depth(), 0);
                assert_eq!(
                    active_escape().unwrap().kind,
                    EscapeKind::Init {
                        owner: Some(callee)
                    }
                );
                let guard = active_guard().unwrap();
                // Inspect without calling escape_target, which pops a guard
                // when it refuses a null Core context.
                assert!(unsafe { (*guard).from_context.is_null() });
                test_mark_active_panicked();
                assert!(test_active_panicked());
            });
            assert_eq!(core_abi_depth(), 1);
            assert_eq!(active_escape().unwrap().owner(), Some(caller));
            assert_eq!(active_escape().unwrap().task(), Some(task));
            assert!(!test_active_panicked());

            with_isolated_destroy_boundary(callee, || {
                assert_eq!(core_abi_depth(), 0);
                assert_eq!(
                    active_escape().unwrap().kind,
                    EscapeKind::Exit { owner: callee }
                );
                test_mark_active_panicked();
            });
            assert_eq!(core_abi_depth(), 1);
            assert_eq!(active_escape().unwrap().owner(), Some(caller));
            assert_eq!(active_escape().unwrap().task(), Some(task));
            assert!(!test_active_panicked());
        });
        assert_eq!(core_abi_depth(), 0);
        enter_anchor();
    }

    /// 验收：IRQ scope 藏在 init 边界下面时，top-guard-only 的
    /// `in_irq_context()` 会漏掉它，祖先遍历不会。
    #[test]
    fn irq_ancestor_stays_forbidden_beneath_a_nested_init_boundary() {
        let _boundary = test_boundary_lock();
        enter_anchor();

        with_irq_scope(ComponentId::from_raw(0xE), || {
            with_test_init_boundary(Some(ComponentId::from_raw(0xF)), || {
                assert!(!in_irq_context(), "top guard is the init boundary");
                assert!(irq_in_chain(), "ancestor walk still finds the IRQ scope");
                assert!(scheduling_forbidden());
            });
        });

        assert!(!scheduling_forbidden());
        enter_anchor();
    }

    /// 验收：service guard 被标记 panicked（boot panic handler 在逃逸前的动作）
    /// 后弹出，仍按后进先出恢复被中断的 caller 边界。
    #[test]
    fn panicked_service_boundary_restores_previous_boundary() {
        let _boundary = test_boundary_lock();
        enter_anchor();
        enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        let provider = ComponentId::from_raw(9);
        with_test_service_boundary(
            provider,
            EndpointId::from_raw(1),
            Some(TaskId::from_raw(7)),
            || {
                assert_eq!(active_escape().unwrap().owner(), Some(provider));
                test_mark_active_panicked();
                assert_eq!(active_escape().unwrap().owner(), Some(provider));
            },
        );

        let info = active_escape().expect("caller boundary restored after the panic");
        assert_eq!(info.owner(), Some(ComponentId::from_raw(3)));
        assert_eq!(info.task(), Some(TaskId::from_raw(7)));
        enter_anchor();
    }

    /// 验收：service call 只把 caller task 记为**执行来源**，绝不改写任务归属；
    /// 边界弹出后 caller 边界原样恢复。
    #[test]
    fn service_call_carries_caller_task_provenance_without_mutating_ownership() {
        let _boundary = test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        crate::task::init();
        enter_anchor();

        // Given: a caller task owned by A.
        let caller = ComponentId::from_raw(0x00C0_FFEE);
        let task = crate::task::get_task_table()
            .lock()
            .create(caller, 0x1000, core::ptr::null_mut())
            .unwrap();
        enter_task(task, caller);

        // When: A's task makes a service call into B.
        let provider = ComponentId::from_raw(0xB0B0);
        with_test_service_boundary(provider, EndpointId::from_raw(7), Some(task), || {
            // Then: the principal is B, with the caller task as provenance...
            let ctx = crate::resource::RequestContext::ambient().expect("service boundary");
            assert_eq!(ctx.component, provider, "provider is the principal");
            assert_eq!(ctx.task, Some(task), "caller task is provenance");
            // ...and the task truth is untouched.
            let table = crate::task::get_task_table().lock();
            let record = table.get(task).expect("caller task still exists");
            assert_eq!(
                record.owner(),
                caller,
                "a service call never re-owns the caller task"
            );
            assert_eq!(record.state(), crate::task::TaskState::Created);
        });

        // Then: the interrupted caller boundary is restored.
        let restored = crate::resource::RequestContext::ambient().expect("task boundary restored");
        assert_eq!(restored.component, caller);
        assert_eq!(restored.task, Some(task));

        crate::task::get_task_table().lock().remove(task).unwrap();
        enter_anchor();
    }

    /// 验收：re-entry 判定覆盖 task owner / service owner / 外层 init/exit；
    /// IRQ owner 不算 service predecessor；调度锚点不被穿越。
    #[test]
    fn provider_in_active_chain_covers_task_service_and_lifecycle_owners() {
        let _boundary = test_boundary_lock();
        enter_anchor();

        let a = ComponentId::from_raw(0xA);
        let b = ComponentId::from_raw(0xB);
        let c = ComponentId::from_raw(0xC);
        let endpoint = EndpointId::from_raw(1);

        enter_task(TaskId::from_raw(7), a);
        assert!(
            provider_in_active_chain(a),
            "running task owner is in the chain"
        );
        assert!(!provider_in_active_chain(b));

        with_test_service_boundary(b, endpoint, Some(TaskId::from_raw(7)), || {
            assert!(
                provider_in_active_chain(b),
                "the provider itself is in the chain"
            );
            assert!(
                provider_in_active_chain(a),
                "the enclosing task owner stays in the chain"
            );
            assert!(!provider_in_active_chain(c));

            with_test_init_boundary(Some(c), || {
                assert!(
                    provider_in_active_chain(c),
                    "an enclosing init instance is in the chain"
                );
                assert!(provider_in_active_chain(b));
                with_irq_scope(ComponentId::from_raw(9), || {
                    assert!(
                        !provider_in_active_chain(ComponentId::from_raw(9)),
                        "an IRQ owner is not a service predecessor"
                    );
                });
            });
        });

        // The scheduling anchor is not traversed: an init boundary that switched
        // into a task is not part of the task's synchronous chain.
        enter_anchor();
        with_test_init_boundary(Some(c), || {
            enter_task(TaskId::from_raw(8), a);
            assert!(provider_in_active_chain(a), "task owner");
            assert!(
                !provider_in_active_chain(c),
                "the anchor's init guard is not part of the task chain"
            );
            enter_anchor();
        });

        enter_anchor();
    }

    unsafe fn test_abandon(_context: *mut u8) -> ! {
        panic!("test context is never abandoned")
    }

    /// 跨 AS 可恢复现场是 **per-invocation + LIFO** 链：嵌套安装 / 恢复互不串扰
    /// （未来 A→B 直接切换的前提）。
    #[test]
    fn cross_as_context_chain_is_per_invocation_lifo() {
        let _boundary = test_boundary_lock();
        let mut a = cross_as::CrossAsContext {
            context: core::ptr::null_mut(),
            abandon: test_abandon,
            expected_satp: 0xA,
            space: None,
        };
        let mut b = cross_as::CrossAsContext {
            context: core::ptr::null_mut(),
            abandon: test_abandon,
            expected_satp: 0xB,
            space: None,
        };

        assert!(cross_as::active_cross_as().is_none());
        let none = cross_as::swap_cross_as(&mut a);
        assert!(none.is_null());
        let active = cross_as::active_cross_as().unwrap();
        // SAFETY: 测试单线程，指针来自上面的有效记录。
        assert_eq!(unsafe { (*active).expected_satp }, 0xA);
        assert!(unsafe { (*active).context.is_null() });
        assert!(unsafe { (*active).space.is_none() });
        assert_eq!(
            unsafe { (*active).abandon } as usize,
            test_abandon as *const () as usize,
            "abandon 出口随记录走"
        );

        let previous = cross_as::swap_cross_as(&mut b);
        assert_eq!(previous, &mut a as *mut _);
        let active = cross_as::active_cross_as().unwrap();
        // SAFETY: 同上。
        assert_eq!(unsafe { (*active).expected_satp }, 0xB);

        cross_as::restore_cross_as(previous);
        let active = cross_as::active_cross_as().unwrap();
        // SAFETY: 同上。
        assert_eq!(unsafe { (*active).expected_satp }, 0xA);

        cross_as::restore_cross_as(none);
        assert!(cross_as::active_cross_as().is_none());
    }
}
