//! Component panic containment: init boundary **and** runtime task boundary.
//!
//! A KernelNative component can panic at three Core boundaries, and all are
//! contained by escaping to a Core-owned context instead of unwinding:
//!
//! 1. **Init boundary** (`kcomp_init`): the component entry runs on a temporary
//!    Core-owned stack ([`call_on_isolated_stack`]).  Its normal return and the
//!    boot panic handler both switch back to the saved caller context; neither
//!    path returns through the component context.
//! 2. **Task boundary**: the scheduler installs an escape guard on every switch
//!    into a component task ([`enter_task`]).  A panic in the task is redirected
//!    to a Core-owned **task-abort context** ([`task_abort_trampoline`]), which
//!    commits the dead task to `Exited`, fails its owning component, and
//!    reschedules in a clean Core context.
//! 3. **Exit boundary** (`kcomp_exit`, graceful stop): the hook runs on the same
//!    temporary Core-owned stack as init ([`call_component_exit`]) and records
//!    the **stopped instance** as its ambient identity.  A panic escapes back
//!    to `component/exit.rs::stop_component`, which classifies it as an exit
//!    failure.
//!
//! Because control never returns through the panicking frame this is **not**
//! Rust unwinding and remains compatible with `panic = "abort"`.
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
use crate::memory;
use crate::task::TaskId;
use arch::{ContextImpl, CpuArch, CpuImpl};
use core::mem::MaybeUninit;

const COMPONENT_STACK_BYTES: usize = 32 * 1024;
const TASK_ABORT_STACK_BYTES: usize = 32 * 1024;
const STACK_ALIGNMENT: usize = 16;
const STACK_ALLOCATION_FAILED: i32 = -12;

type ComponentEntry = extern "C" fn() -> i32;

/// Result of invoking a component entry on its isolated stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    Returned(i32),
    Panicked,
}

/// Which Core boundary an active escape guard protects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeKind {
    /// `kcomp_init` running on a temporary Core stack.  The owner is the
    /// component Core is initializing, or `None` for a direct (selftest) call.
    Init { owner: Option<ComponentId> },
    /// `kcomp_exit` running on a temporary Core stack during a graceful stop.
    ///
    /// The owner is the **instance being stopped** — never the monitor or other
    /// component that initiated the stop — so identity-sensitive Core calls made
    /// by the hook are attributed to the stopped instance.
    Exit { owner: ComponentId },
    /// A component task running on its own kernel stack.
    Task { task: TaskId, owner: ComponentId },
}

/// Lock-free snapshot of the active escape guard, for boot diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscapeInfo {
    pub kind: EscapeKind,
}

impl EscapeInfo {
    /// Owning component, when known (`Task` / `Exit` always, `Init` only inside
    /// a load).
    pub fn owner(self) -> Option<ComponentId> {
        match self.kind {
            EscapeKind::Init { owner } => owner,
            EscapeKind::Exit { owner } => Some(owner),
            EscapeKind::Task { owner, .. } => Some(owner),
        }
    }

    /// Running task id, or `None` at the init and exit boundaries.
    pub fn task(self) -> Option<TaskId> {
        match self.kind {
            EscapeKind::Init { .. } | EscapeKind::Exit { .. } => None,
            EscapeKind::Task { task, .. } => Some(task),
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
    /// Init only: entry run by the trampoline on normal return.
    entry: Option<ComponentEntry>,
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

/// Calls an entry after deriving its C ABI function pointer from a loaded ELF image.
pub fn call_component_init(entry: usize) -> CallOutcome {
    // SAFETY: `loader::load_component` validates and relocates `kcomp_init`
    // before its entry address reaches this Core-only function.
    let init = unsafe { core::mem::transmute::<usize, ComponentEntry>(entry) };
    call_on_isolated_stack(init)
}

/// Calls a component **exit hook** (`kcomp_exit`) on a Core-owned stack.
///
/// The guard records `owner` — the instance being stopped — as the ambient
/// identity, so `RequestContext::ambient()` inside the hook resolves to that
/// instance (never to the monitor/caller that initiated the stop, and never to
/// `load::current_component()`).  Panic routing is the same as init: the escape
/// switches back to `stop_component`, which classifies the outcome.
pub fn call_component_exit(entry: usize, owner: ComponentId) -> CallOutcome {
    // SAFETY: `entry` comes from `LoadedComponent::exit`, which the loader
    // resolved and relocated from the component's own symbol table (same
    // contract as `call_component_init`).
    let exit = unsafe { core::mem::transmute::<usize, ComponentEntry>(entry) };
    call_on_isolated_stack_with(exit, EscapeKind::Exit { owner })
}

/// Calls a component entry on a Core-owned stack and contains its panic escape.
pub fn call_on_isolated_stack(entry: ComponentEntry) -> CallOutcome {
    call_on_isolated_stack_with(
        entry,
        EscapeKind::Init {
            owner: crate::component::load::current_component(),
        },
    )
}

/// Shared body of the init / exit boundaries: allocate a Core-owned stack,
/// install `kind` as the active escape guard, switch, and collect the outcome.
fn call_on_isolated_stack_with(entry: ComponentEntry, kind: EscapeKind) -> CallOutcome {
    let stack = match memory::alloc_region(COMPONENT_STACK_BYTES) {
        Ok(stack) => stack,
        Err(_) => return CallOutcome::Returned(STACK_ALLOCATION_FAILED),
    };
    let stack_top = (stack.base() + stack.size()) & !(STACK_ALIGNMENT - 1);
    let mut core_context = CpuImpl::new_context(0, 0);
    let mut component_context = CpuImpl::new_context(trampoline as *const () as usize, stack_top);
    let mut guard = EscapeGuard {
        kind,
        from_context: &mut component_context,
        to_context: &mut core_context,
        entry: Some(entry),
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
    let _ = memory::free_region(stack);
    outcome
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
            entry: None,
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
    // SAFETY: the active guard belongs to the caller frame suspended by
    // `call_on_isolated_stack`; this context is the only execution using it.
    let guard = unsafe { &mut *guard_ptr };
    guard.returned = match guard.entry {
        Some(entry) => entry(),
        None => 0,
    };
    switch_to_core(guard)
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
/// recorded in the guard: the saved caller for init, the task-abort context for
/// a task.  Never returns to the escaping frame.
fn switch_to_core(guard: &mut EscapeGuard) -> ! {
    // SAFETY: both context records live in the suspended caller frame of
    // `call_on_isolated_stack`; `from_context` is the current context and
    // `to_context` was saved immediately before this component began.
    unsafe {
        CpuImpl::context_switch(&mut *guard.from_context, &*guard.to_context);
    }
    halt()
}

/// Escapes an active component panic without allocation, logging, or locking.
///
/// Both the init and the task boundary escape through `from_context` →
/// `to_context`; the resumed Core context performs the containment bookkeeping.
/// Returns `false` when the panic originated outside an isolated component.
/// A `true` result is unreachable in a functioning context backend because the
/// switch resumes the Core context instead of this panic handler.
pub fn panic_escape() -> bool {
    let Some(guard_ptr) = active_guard() else {
        return false;
    };
    // SAFETY: `guard_ptr` is installed by the suspended caller/scheduler on this
    // CPU and remains valid until it is replaced after the switch back.
    let guard = unsafe { &mut *guard_ptr };
    guard.state.mark_panicked();
    switch_to_core(guard)
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
/// [`call_on_isolated_stack`], so host tests can exercise the boundary nesting
/// that principal resolution depends on.
#[cfg(test)]
pub(crate) fn with_test_init_boundary<R>(owner: Option<ComponentId>, f: impl FnOnce() -> R) -> R {
    let mut from_context = CpuImpl::new_context(0, 0);
    let mut to_context = CpuImpl::new_context(0, 0);
    let mut guard = EscapeGuard {
        kind: EscapeKind::Init { owner },
        from_context: &mut from_context,
        to_context: &mut to_context,
        entry: None,
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
/// a context switch.  Mirrors the guard installed by [`call_component_exit`], so
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
        entry: None,
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
    fn exit_boundary_is_the_ambient_identity_and_not_a_publish_principal() {
        // Given: no ambient boundary.
        let _boundary = test_boundary_lock();
        enter_anchor();
        assert!(active_escape().is_none());

        // When: an exit boundary for component 5 is active (as during kcomp_exit).
        let owner = ComponentId::from_raw(5);
        with_test_exit_boundary(owner, || {
            // Then: Core calls are attributed to the stopped instance...
            let ambient = crate::handle::RequestContext::ambient().expect("ambient identity");
            assert_eq!(ambient.component, owner);
            assert_eq!(ambient.task, None);
            // ...and publish remains an init-time operation.
            assert!(crate::handle::RequestContext::ambient_init().is_none());
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
}
