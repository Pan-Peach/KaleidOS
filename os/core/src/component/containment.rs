//! Component panic containment: init boundary, runtime task boundary, and the
//! component→component **service-call boundary**.
//!
//! A KernelNative component can panic at four Core boundaries, and all are
//! contained by escaping to a Core-owned context instead of unwinding:
//!
//! 1. **Create boundary** (`kcomp_instance_create`): the component entry runs on
//!    a temporary Core-owned stack ([`call_component_create`]).  Its normal
//!    return and the boot panic handler both switch back to the saved caller
//!    context; neither path returns through the component context.
//! 2. **Task boundary**: the scheduler installs an escape guard on every switch
//!    into a component task ([`enter_task`]).  A panic in the task is redirected
//!    to a Core-owned **task-abort context** ([`task_abort_trampoline`]), which
//!    commits the dead task to `Exited`, fails its owning component, and
//!    reschedules in a clean Core context.
//! 3. **Destroy boundary** (`kcomp_instance_destroy`, graceful stop): the hook
//!    runs on the same temporary Core-owned stack as create
//!    ([`call_component_destroy`]) and records the **stopped instance** as its
//!    ambient identity.  A panic escapes back to
//!    `component/exit.rs::stop_component`, which classifies it as a destroy
//!    failure.
//! 4. **Service-call boundary** (`kcore_endpoint_call`): the provider's
//!    `kcomp_service_dispatch` runs on its own temporary Core-owned **service
//!    stack** ([`call_component_service`]) under the **provider's** principal.
//!    A normal return and a panic both switch back to the calling Core frame,
//!    which classifies the outcome; a panicked provider is failed by
//!    `component/call.rs`, never the caller.
//!
//! Because control never returns through the panicking frame this is **not**
//! Rust unwinding and remains compatible with `panic = "abort"`.
//!
//! # Service-call boundary
//!
//! [`call_component_service`] layers one [`EscapeKind::ServiceCall`] guard over
//! the caller's boundary for exactly one dispatcher invocation:
//!
//! - **Principal**: `RequestContext::ambient()` inside the dispatcher resolves to
//!   the **provider**, with `task = caller_task` recorded as execution
//!   provenance — never as authority over that task.  `ambient_init()` is `None`
//!   inside the boundary: a service call must not confer create-time publication
//!   permission.
//! - **Escapable**: a dispatcher panic switches to the suspended caller frame
//!   (the same save/restore discipline as create/destroy), so
//!   `component/call.rs` can fail the provider and return a transport error while
//!   the caller stays alive.  The abandoned service stack is **retained**
//!   (phase-1 conservative residency, explicit `mem::forget` of the lease — no
//!   allocator lock on the panic path).
//! - **Scheduling-forbidden**: the provider has no scheduler-visible task, so
//!   [`scheduling_forbidden`] rejects `sched::run` / `yield_current` /
//!   `exit_current` and task creation for the **whole chain** — including beneath
//!   a nested lifecycle boundary (`Service → create → sched::run` can no longer
//!   slip past a top-guard-only check).
//! - **Re-entry**: `component/call.rs` rejects a call whose provider already runs
//!   in the active synchronous chain ([`provider_in_active_chain`]) — the
//!   scheduling anchor is deliberately not traversed.
//! - **Interrupt state**: the RISC-V context record holds `ra` / `sp` / `s0-s11`
//!   only, so the service boundary saves and restores the interrupt-enable state
//!   explicitly around the switch.
//!
//! # Phase-1 limitation: real dispatch is QEMU-only
//!
//! The host fake context backend does not execute component entry bodies, so the
//! **real** stack switch, a **real** provider panic, and the end-to-end
//! A → B → C principal order in actual execution cannot be proven by `cargo
//! test`.  They must be proven on QEMU with a real component that exports
//! `kcomp_service_dispatch` (a later step).  Host tests exercise the boundary
//! bookkeeping through the test-only helpers ([`with_test_service_boundary`],
//! [`test_mark_active_panicked`]) instead.
//!
//! # Ambient escape guard (lock-free)
//!
//! [`ACTIVE_GUARD`] holds the escape record of the execution that is currently
//! allowed to panic into containment.  It is a plain static pointer, never a
//! lock: the boot panic handler performs no allocation, logging, or locking, so
//! it cannot acquire one.  Phase 1 is single-active-CPU, so the record is only
//! touched at synchronous entry/switch boundaries on that CPU.
//!
//! Nesting is tracked for init guards via [`GuardState::previous`].  At the
//! scheduler boundary the ambient guard is saved once ([`enter_task`]) and
//! restored when control returns to the anchor ([`enter_anchor`]); this also
//! preserves an enclosing init guard when a component task drives the scheduler.
//!
//! # IRQ attribution scope
//!
//! [`with_irq_scope`] layers one more boundary over the active guard around one
//! component IRQ callback ([`crate::irq::on_external`]): the principal is the
//! **IRQ line's owner** (Core truth from the routing table) with `task = None`,
//! never the interrupted execution.  The scope stores the guard it replaced and
//! restores it **explicitly** (same discipline as the create/destroy boundaries);
//! nested scopes restore in order.  It is **bookkeeping for trusted
//! KernelNative components, not an authentication boundary** — it records who
//! Core is dispatching for, it cannot prove the callback code really belongs to
//! that owner.
//!
//! An IRQ scope is synchronous and non-yielding: scheduler-affecting Core calls
//! are rejected while it is active or anywhere beneath it
//! ([`scheduling_forbidden`]).  It is also **not
//! escapable**: [`panic_escape`] restores the interrupted guard and refuses,
//! because an IRQ callback has no Core-owned context to resume and escaping
//! into the interrupted task would misattribute the callback's panic.  A panic
//! inside an IRQ scope therefore stays fatal.
//!
//! # Diagnostics
//!
//! [`active_escape`] is the lock-free read side used by the boot panic handler;
//! [`write_escape_line`] renders one short line to a direct (lock-free) writer.
//! Panics outside an active guard keep the existing fatal Core path.
//!
//! # Phase-1 limitations (documented, intentional)
//!
//! - The failed component's image, allocations, and abandoned stack frames stay
//!   resident; only authority is revoked via `fail_component`.  The aborted
//!   task's kernel stack is **not** reclaimed.
//! - A panicked service call's Core-owned service stack is **not** reclaimed
//!   either (explicit `mem::forget`; see [`call_component_service`]).  A service
//!   stack is freed only on the normal-return path.
//! - Other tasks owned by the failed component are **not** force-stopped:
//!   `may_run` excludes them from runnable candidates (they never run again),
//!   but their records stay non-`Exited` because Core has no task-stop API yet.
//!   The graceful-stop path therefore refuses components that still own live
//!   tasks (see `component/exit.rs`).
//! - No Core lock may span the switch.  A panic while a Core lock is held can
//!   still leave that lock held (known KernelNative limitation).
//! - The task-abort context is single-CPU and reused; it is only entered once
//!   per panic and never resumed.  It runs on its own 32 KiB Core stack, so the
//!   abort bookkeeping does not consume the dead task's stack.
//! - Component task stacks are `memory::ALLOC_GRANULE` (4 KiB, see
//!   `task::TaskTable::create`); the panic diagnostic shares that stack.  It is
//!   adequate for the current tests, but a larger component-task stack may be
//!   warranted once components do more work before panicking.

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
    /// 一次 component→component 服务调用（provider 的
    /// `kcomp_service_dispatch`，参数全部来自 `component/call.rs` 的锁内拷贝）。
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
    /// ([`crate::irq::on_external`]).
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
}

impl EscapeKind {
    /// Whether a panic in this boundary escapes to the Core-owned context
    /// recorded in the guard ([`panic_escape`]).
    ///
    /// An IRQ attribution scope has no Core-owned context to resume and must
    /// stay fatal; every other boundary (init / exit / task / service call) was
    /// entered through a saved Core context and can escape to it.
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
    /// Owning component, when known (`Task` / `Exit` / `Irq` / `ServiceCall`
    /// always, `Init` only inside a load).
    pub fn owner(self) -> Option<ComponentId> {
        match self.kind {
            EscapeKind::Init { owner } => owner,
            EscapeKind::Exit { owner } => Some(owner),
            EscapeKind::Task { owner, .. } => Some(owner),
            EscapeKind::Irq { owner } => Some(owner),
            EscapeKind::ServiceCall { owner, .. } => Some(owner),
        }
    }

    /// Running task id, or `None` at the init, exit, and IRQ boundaries.
    ///
    /// A service call reports the **caller task it originated from** (execution
    /// provenance); it does not make the provider that task's owner.
    pub fn task(self) -> Option<TaskId> {
        match self.kind {
            EscapeKind::Init { .. } | EscapeKind::Exit { .. } | EscapeKind::Irq { .. } => None,
            EscapeKind::Task { task, .. } => Some(task),
            EscapeKind::ServiceCall { caller_task, .. } => caller_task,
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
}

// Phase 1 is single-active-CPU and component entry is synchronous, so the
// active record is accessed only by the current component execution or its
// panic handler.  It intentionally uses no lock: the panic handler cannot
// acquire one.
static mut ACTIVE_GUARD: *mut EscapeGuard = core::ptr::null_mut();
/// Ambient guard saved when the scheduler first leaves the anchor for a task.
static mut ANCHOR_GUARD: *mut EscapeGuard = core::ptr::null_mut();
/// True while the CPU is inside the task-scheduling region.
static mut TASK_REGION: bool = false;

/// Dedicated stack for the task-abort trampoline: it runs after the dead task's
/// stack is abandoned, so it must have its own.
#[repr(align(16))]
struct TaskAbortStack([u8; TASK_ABORT_STACK_BYTES]);
static mut TASK_ABORT_STACK: TaskAbortStack = TaskAbortStack([0; TASK_ABORT_STACK_BYTES]);
/// Core-owned context that receives a panicking task's registers.
static mut TASK_SCRATCH_CONTEXT: MaybeUninit<ContextImpl> = MaybeUninit::uninit();
/// Core-owned context whose entry is [`task_abort_trampoline`].
static mut TASK_ABORT_CONTEXT: MaybeUninit<ContextImpl> = MaybeUninit::uninit();
/// Persistent task escape record (single CPU, overwritten per task switch).
static mut TASK_GUARD: MaybeUninit<EscapeGuard> = MaybeUninit::uninit();

/// Prepare the Core-owned task-abort context.  Called once from `core::init`
/// before any task can run.
pub fn init() {
    let context = CpuImpl::new_context(
        task_abort_trampoline as *const () as usize,
        abort_stack_top(),
    );
    // SAFETY: [Category 1 — Initialization] this runs once during `core::init`,
    // before the scheduler is reachable; no other CPU exists in phase 1.
    unsafe {
        core::ptr::addr_of_mut!(TASK_ABORT_CONTEXT).write(MaybeUninit::new(context));
    }
}

fn abort_stack_top() -> usize {
    // SAFETY: [Category 1 — Initialization] only forms the one-past-end address
    // of the static abort stack; no reference is created.
    let base = unsafe { core::ptr::addr_of_mut!(TASK_ABORT_STACK.0) as usize };
    (base + TASK_ABORT_STACK_BYTES) & !(STACK_ALIGNMENT - 1)
}

fn replace_active(next: *mut EscapeGuard) -> Option<*mut EscapeGuard> {
    // SAFETY: [Category 2 — Data races] phase 1 has one active CPU and this
    // pointer is changed only at synchronous entry/return boundaries; the
    // panic handler runs on that same CPU and performs no nested mutation.
    let previous = unsafe { core::ptr::replace(core::ptr::addr_of_mut!(ACTIVE_GUARD), next) };
    (!previous.is_null()).then_some(previous)
}

fn active_guard() -> Option<*mut EscapeGuard> {
    // SAFETY: [Category 2 — Data races] the single-active-CPU contract used by
    // `replace_active` also serializes this raw read with guard installation.
    let guard = unsafe { core::ptr::addr_of!(ACTIVE_GUARD).read() };
    (!guard.is_null()).then_some(guard)
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
        // SAFETY: [Category 2 — Data races] phase 1 is single-active-CPU; the
        // chain is stable while this synchronous walk runs (no guard is popped
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
/// (synchronous, non-yielding top half) or a service call (the provider runs on
/// a Core-owned service stack under its own principal — there is no
/// scheduler-visible task, and switching away would abandon the service stack).
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
            EscapeKind::Irq { .. } | EscapeKind::ServiceCall { .. }
        )
    })
}

/// Whether `provider` already runs in the active synchronous chain: as the owner
/// of a task guard, as the owner of any service-call guard, or as the instance
/// being created / destroyed by an enclosing init / exit guard.
///
/// `component/call.rs` consults this before dispatching: a synchronous call back
/// into an instance that is already on the current chain is re-entry, not a
/// service request.  IRQ scopes are intentionally not counted — an IRQ callback
/// cannot reach a generic service call at all (the IRQ chain gate rejects it
/// first).
pub(crate) fn provider_in_active_chain(provider: ComponentId) -> bool {
    chain_any(|guard| match guard.kind {
        EscapeKind::Task { owner, .. } | EscapeKind::ServiceCall { owner, .. } => owner == provider,
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
    // SAFETY: `entry` comes from `ComponentImage::destroy`, which the loader
    // resolved and relocated from the component's own symbol table (same
    // contract as `call_component_create`).
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
    let mut core_context = CpuImpl::new_context(0, 0);
    let mut component_context = CpuImpl::new_context(trampoline as *const () as usize, stack_top);
    let mut guard = EscapeGuard {
        kind,
        from_context: &mut component_context,
        to_context: &mut core_context,
        call,
        returned: 0,
        state: GuardState::new(None),
    };
    guard.state = GuardState::new(replace_active(&mut guard));

    // The context records are local to this suspended caller frame and remain
    // valid until the component returns or `panic_escape` resumes this point.
    CpuImpl::context_switch(&mut core_context, &component_context);

    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    let outcome = match guard.state.panicked() {
        true => CallOutcome::Panicked,
        false => CallOutcome::Returned(guard.returned),
    };
    IsolatedRun {
        outcome,
        stack: Some(stack),
    }
}

/// Runs `f` inside an **IRQ attribution scope** ([`EscapeKind::Irq`]): Core
/// calls made by `f` resolve to `owner` with `task = None`.
///
/// Installed by [`crate::irq::on_external`] around exactly one component
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
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
    // Explicit restore (no unwinding; see module docs).  The record captured
    // the guard it replaced, so nested IRQ scopes pop in order.
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
pub fn enter_task(task: TaskId, owner: ComponentId) {
    // SAFETY: [Category 2 — Data races] single active CPU; called on the
    // synchronous scheduling path with all locks released.
    unsafe {
        if !TASK_REGION {
            ANCHOR_GUARD = ACTIVE_GUARD;
            TASK_REGION = true;
        }
        let guard = EscapeGuard {
            kind: EscapeKind::Task { task, owner },
            from_context: core::ptr::addr_of_mut!(TASK_SCRATCH_CONTEXT).cast::<ContextImpl>(),
            to_context: core::ptr::addr_of_mut!(TASK_ABORT_CONTEXT).cast::<ContextImpl>(),
            call: IsolatedCall::None,
            returned: 0,
            state: GuardState::new(None),
        };
        core::ptr::addr_of_mut!(TASK_GUARD).write(MaybeUninit::new(guard));
        ACTIVE_GUARD = core::ptr::addr_of_mut!(TASK_GUARD).cast::<EscapeGuard>();
    }
}

/// Scheduler hook: restore the ambient guard before switching **into the anchor**.
pub fn enter_anchor() {
    // SAFETY: [Category 2 — Data races] single active CPU; synchronous path.
    unsafe {
        TASK_REGION = false;
        ACTIVE_GUARD = ANCHOR_GUARD;
        ANCHOR_GUARD = core::ptr::null_mut();
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
            // SAFETY: `entry` comes from `ComponentImage::destroy` (loader
            // validated + relocated the symbol); `state` is the instance's
            // opaque pointer (Core never dereferences it).
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
            // SAFETY: `dispatcher` comes from `ComponentImage::service_dispatch`
            // (loader validated + relocated the symbol); `instance_state` /
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
    crate::sched::abort_current_task(task, owner)
}

/// Transfers control from the escaping execution to the Core-owned context
/// recorded in the guard: the saved caller for init / exit / service call, the
/// task-abort context for a task.  Never returns to the escaping frame.
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

/// Escapes an active component panic without allocation, logging, or locking.
///
/// Every escapable boundary (init / exit / task / service call) escapes through
/// `from_context` → `to_context`; the resumed Core context performs the
/// containment bookkeeping.  Returns `false` when the panic originated outside an
/// isolated component, or inside an IRQ attribution scope ([`with_irq_scope`]) —
/// an IRQ callback has no Core context to resume, so its panic stays fatal (the
/// interrupted guard is restored before returning).  A `true` result is
/// unreachable in a functioning context backend because the switch resumes the
/// Core context instead of this panic handler.
pub fn panic_escape() -> bool {
    let Some(guard_ptr) = active_guard() else {
        return false;
    };
    // SAFETY: `guard_ptr` is installed by the suspended caller/scheduler on this
    // CPU and remains valid until it is replaced after the switch back.
    let kind = unsafe { (*guard_ptr).kind };
    if !kind.is_escapable() {
        // An IRQ attribution scope has no Core-owned context to resume, and
        // escaping into the interrupted task would misattribute the callback's
        // panic to a task that did not panic.  Restore the interrupted guard
        // (explicit recovery — never rely on `Drop`, there is no unwinding) and
        // report the panic as uncontained: it stays fatal.  A stale IRQ scope
        // therefore cannot outlive the escape attempt.
        // SAFETY: [Category 2 — Data races] the record is live and this is the
        // only execution touching it; `previous` is a plain pointer copy.
        let previous = unsafe { (*guard_ptr).state.previous() };
        let _ = replace_active(previous.unwrap_or(core::ptr::null_mut()));
        return false;
    }
    // SAFETY: [Category 2 — Data races] short raw access; the record is live and
    // `switch_to_core` never returns to this frame.
    unsafe { (*guard_ptr).state.mark_panicked() };
    switch_to_core(guard_ptr)
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
#[cfg(test)]
static TEST_BOUNDARY_LOCK: spin::Mutex<()> = spin::Mutex::new(());

/// Acquires the test-only active-boundary lock.
#[cfg(test)]
pub(crate) fn test_boundary_lock() -> spin::MutexGuard<'static, ()> {
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
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
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
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
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
    };
    guard.state = GuardState::new(replace_active(&mut guard));
    let result = f();
    let previous = match guard.state.previous() {
        Some(previous) => previous,
        None => core::ptr::null_mut(),
    };
    let _ = replace_active(previous);
    result
}

/// Marks the active escape as panicked, as the boot panic handler does before
/// escaping.  Lets host tests assert that popping a panicked guard still
/// restores the previous boundary.
#[cfg(test)]
pub(crate) fn test_mark_active_panicked() {
    if let Some(guard_ptr) = active_guard() {
        // SAFETY: [Category 2 — Data races] single active CPU / test boundary
        // lock; the record is live and only this test mutates it.
        unsafe { (*guard_ptr).state.mark_panicked() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

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
        // 手工锚定值 = 8 字节 ASCII tag `b"KCOMPABI"` 的大端读数；单一来源是
        // `abi/component.toml`（生成到 generated/abi.rs），这里独立钉死数值。
        assert_eq!(KCOMP_ABI, 0x4B43_4F4D_5041_4249);
        assert_eq!(&KCOMP_ABI.to_be_bytes(), b"KCOMPABI");
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
}
