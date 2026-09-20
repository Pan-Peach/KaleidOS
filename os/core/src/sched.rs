//! 调度 commit 路径（Core 侧）：收集 Runnable 真相 → 请求 SchedulerPolicy
//! 提议 → Core 验证 → commit 状态 → context switch。
//!
//! 这就是 KaleidOS 最有代表性的链：**Policy proposes, Core validates and
//! commits**。调度器组件（如 scheduler_rr）只看到 runnable id 列表（Core
//! 提供的、经过裁剪的输入），只能"提议"下一个 TaskId；存在性、状态、
//! 切换由 Core 验证后生效。
//!
//! # 执行流（单 CPU，phase 1）
//!
//! 组件 init（或 monitor）在**锚点栈**上运行；`run()` 首次进入调度时，
//! 锚点上下文被捕获保存。任务在自己的内核栈上运行；yield/exit 触发
//! `schedule_next`，选下一个任务或（没有 Runnable 时）切回锚点——
//! `run()` 在锚点上下文"返回"，调用者继续。
//!
//! # 锁纪律（关键）
//!
//! `cpu` / `task_table` / `interfaces`+`registry` 三把锁只在**决定阶段**
//! 短暂持有；`context_switch` 必须在全部锁释放后执行——否则切过去的任务
//! 第一次调 yield 就会自死锁（spin::Mutex 不可重入）。
//! 决定阶段与切换之间无 yield 点（单 CPU 协作式），raw 指针安全。
//! 跨 CPU 状态机、Running(cpu) 互斥留给 SMP 里程碑。

use crate::component::interface::{InterfaceAbi, InterfaceKind, get_interfaces};
use crate::component::load::ComponentLoadError;
use crate::component::{ComponentId, containment, registry};
use crate::irq::IrqSaveGuard;
use crate::machine::CpuId;
use crate::task::{self, TaskId, TaskState};
use alloc::boxed::Box;
use alloc::vec::Vec;
use arch::{ContextImpl, CpuArch, CpuImpl};
use spin::{Mutex, Once};

/// SchedulerPolicy 的 function table（与组件 scheduler_rr 重复定义——A/B 双侧
/// ABI 契约，见 docs/component-model.md；组件替换 = 换 provider 实现同一 layout）。
///
/// `api` 指向本 struct；`ctx`（provider opaque state）由 Core 从 binding 单独取出
/// 后原样传入 `choose_next`，**不再放在 vtable 内**。
#[repr(C)]
pub struct SchedulerPolicyApi {
    pub choose_next:
        extern "C" fn(ctx: *mut (), runnable: *const u32, count: usize, current: u32) -> u32,
}

/// SchedulerPolicy 的 exact ABI fingerprint。provider 与 consumer 必须使用完全
/// 相同的值（不一致 → `bind` 拒绝）。
///
/// TODO(service-abi): 未来由 `kcomp-sdk` 统一定义具体 Service contract 的
/// fingerprint；当前为占位值。组件侧镜像定义见
/// `kcomp-sdk::binding::SCHEDULER_POLICY_ABI`（A/B 双侧手工锚定）。
pub const SCHEDULER_POLICY_ABI: InterfaceAbi = InterfaceAbi::from_raw(0x5343_4845_4455_4C52);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedError {
    /// 没有绑定的 SchedulerPolicy（`scheduler`/Policy 未 publish 或 provider 已 Failed）。
    NoPolicy,
    /// 任务表状态机拒绝推进（yield/exit 时当前任务不是 Running 等）。
    ///
    /// 也用于**上下文种类拒绝**：调度操作不得在 IRQ 回调作用域内执行
    /// （`containment::in_irq_context`，见 [`deny_in_irq_context`]），ABI 上是
    /// `-EINVAL`。复用本变体是为了不改内部错误枚举与唯一的 errno 映射表。
    InvalidTransition,
    /// 当前任务从表中消失（Core 不变式被破坏，不应发生）。
    NotFound,
    /// yield/exit 调用时本 CPU 没有在跑任务（只有任务能 yield/exit）。
    NoCurrent,
}

/// 上下文种类门禁：IRQ 回调是同步、不可 yield 的顶半部，调度操作在里面一律
/// 拒绝（返回 `-EINVAL`，绝不 panic），因为 `schedule_next` 会在 trap 上下文里
/// 做 context switch、且没有 Core 拥有的恢复点。Core 机制层就拒绝，ABI 边界
/// （`component/export.rs`）保持原样。
fn deny_in_irq_context() -> Result<(), SchedError> {
    if containment::in_irq_context() {
        return Err(SchedError::InvalidTransition);
    }
    Ok(())
}

/// 本 CPU 的调度真相：锚点上下文 + 当前任务。
///
/// `anchor` = 任务之外执行流（monitor / 组件 init 调用栈）的挂起上下文；
/// 全部任务退出后 CPU 回到这里。首次 `run()` 时捕获，之后每次耗尽任务
/// 都回到同一份（Box 地址稳定，跨切换有效）。
struct CpuState {
    anchor: Option<Box<ContextImpl>>,
    current: Option<TaskId>,
}

static CPU: Once<Mutex<CpuState>> = Once::new();

pub fn init() {
    CPU.call_once(|| {
        Mutex::new(CpuState {
            anchor: None,
            current: None,
        })
    });
}

fn cpu() -> &'static Mutex<CpuState> {
    CPU.get().expect("sched not initialized")
}

/// 当前 CPU 正在运行的任务。没有进入任务执行流时返回 `None`（锚点上下文）。
///
/// 这是 Core 读取执行身份的入口；组件不能通过它修改调度状态。
pub fn current_task() -> Option<TaskId> {
    cpu().lock().current
}

/// 收集全部 Runnable 且 **owner 仍是活实例**的任务（BTreeMap 迭代序 = id 升序；
/// 列表内容由 Core 决定，调度器只读这份裁剪过的输入）。
///
/// 组件失败 = 逻辑死亡：`Failed` 组件的任务必须从候选中剔除，否则调度器会把 CPU
/// 交给一个已经死掉的实例。两把锁**先后分开**取（先 task 表快照 owner、再 registry
/// 判定），不做嵌套，避免与 create_task（registry → task_table）的锁序冲突。
fn collect_runnable() -> Vec<TaskId> {
    let candidates: Vec<(TaskId, ComponentId)> = {
        let table = task::get_task_table().lock();
        table
            .iter()
            .filter(|(_, r)| r.state() == TaskState::Runnable)
            .map(|(id, r)| (*id, r.owner()))
            .collect()
    };
    let reg = registry::get_registry().lock();
    candidates
        .into_iter()
        .filter(|(_, owner)| reg.may_run(*owner))
        .map(|(id, _)| id)
        .collect()
}

/// Commit-time 门禁：任务 owner 此刻是否仍允许运行。
///
/// `collect_runnable` 只过滤一次候选；在真正把 CPU 交给某个任务前，Core 用同一条
/// 真相（`Registry::may_run`）再验证一次——`pick_next` 可能隔离了一个失败组件，
/// 而它恰好是候选任务的 owner。owner 已死 → 绝不 commit。
fn owner_still_runnable(id: TaskId) -> bool {
    let owner = task::get_task_table().lock().get(id).map(|r| r.owner());
    owner.is_some_and(crate::component::may_run)
}

/// 解析绑定的 SchedulerPolicy。锁序 registry → interfaces（与 publish
/// 路径一致，见 component/load.rs）。返回 provider + `api`/`ctx`（Core 不解引用）。
fn resolve_policy() -> Result<(ComponentId, *const (), *mut ()), SchedError> {
    let reg = registry::get_registry().lock();
    let ifs = get_interfaces().lock();
    let view = ifs
        .bind(
            &reg,
            b"scheduler",
            InterfaceKind::Policy,
            SCHEDULER_POLICY_ABI,
        )
        .map_err(|_| SchedError::NoPolicy)?;
    Ok((view.provider, view.api, view.ctx))
}

/// 请求策略提议下一个任务。返回 None = 没有可运行任务（回锚点）。
///
/// 提议非法（契约不符 / 提议 id 不在 runnable 列表）→ provider 被标
/// `Failed`（隔离错误组件），Core 退化到确定性回退（id 序首项）——
/// 一个完全错误的调度器组件不能挂起调度，也不能把 CPU 交给不存在的任务。
fn pick_next(runnable: &[TaskId]) -> Result<Option<TaskId>, SchedError> {
    if runnable.is_empty() {
        return Ok(None);
    }
    let (provider, api, ctx) = resolve_policy()?;
    // SAFETY: api 由 provider 的 staged publish 写入（组件的静态 function table），
    // provider Ready 与 exact ABI 校验已在 bind 内完成；table 在其组件存活期内有效。
    let vtable = unsafe { &*(api as *const SchedulerPolicyApi) };
    let ids: Vec<u32> = runnable.iter().map(|id| id.raw()).collect();
    let current = cpu().lock().current.map_or(u32::MAX, |id| id.raw());
    let proposed = (vtable.choose_next)(ctx, ids.as_ptr(), ids.len(), current);

    let proposed = TaskId::from_raw(proposed);
    crate::trace::emit(crate::trace::TraceEvent::PolicyProposal {
        component: provider,
        task: proposed,
    });
    if runnable.contains(&proposed) {
        crate::trace::emit(crate::trace::TraceEvent::PolicyAccepted {
            component: provider,
            task: proposed,
        });
        return Ok(Some(proposed));
    }
    // 组件提出非法提议：隔离 + 回退（Core 不被错误组件挂起）。
    crate::trace::emit(crate::trace::TraceEvent::PolicyRejected {
        component: provider,
        reason: crate::trace::RejectReason::NotRunnable,
    });
    registry::get_registry().lock().mark_failed(provider).ok();
    Ok(Some(runnable[0]))
}

/// 核心切换：from（当前任务或锚点）→ to（策略提议或锚点）。
///
/// `from = Some(id)`：把当前任务推进到 `after`（yield→Runnable / exit→Exited）
/// 并保存它的上下文；`from = None`：捕获锚点上下文（首次 run）。
/// `next = None`：没有可运行任务，切回锚点。
///
/// `abort` 非空时走**任务 panic 收尾路径**：任务已 commit 为 `Exited`
/// 后、切换前调用 `fail_component`。顺序刻意如此——`pick_next` 先于
/// `fail_component`，这样即使 panic 的任务属于当前 scheduler provider，
/// 后继任务也已在 provider 被解绑前选定，Core 不会被失败组件挂起。
///
/// Failed 门禁发生在**候选收集**与 **commit 之前**（Phase 0）：已经 `Failed` 的
/// owner 的任务既不进候选、也过不了 commit-time 复验。abort 交接是唯一例外——
/// 后继任务在 owner 死亡前就已完成 commit，Core 靠它保持存活。
///
/// 锁纪律：`context_switch` 前全部锁释放；锁外先 revoke、再按**目标上下文**
/// 安装逃逸 guard，最后切换。
fn schedule_next(
    from: Option<TaskId>,
    after: Option<TaskState>,
    abort: Option<(TaskId, ComponentId)>,
) -> Result<(), SchedError> {
    let guard = IrqSaveGuard::new();
    // Phase 0：收集 + 提议（interfaces/registry 锁在 resolve_policy 内，短暂）
    let runnable = collect_runnable();
    let mut next = pick_next(&runnable)?;

    // Commit-time 门禁：候选过滤只发生一次；真正 commit 前再核对一次 owner 真相
    //（`pick_next` 可能隔离了一个失败的调度器 provider，而它恰好是候选的 owner）。
    // owner 已死 → 回锚点，绝不把 CPU 交给已死实例的任务。
    if let Some(id) = next
        && !owner_still_runnable(id)
    {
        next = None;
    }

    // Phase 1：锁内 commit 状态 + 取上下文指针
    //
    // TODO(C5): 抢占安全——时钟中断可能在本临界区内打断（被打破的上下文
    //   持有 cpu/table 锁时，trap 处理器再取同样的锁 = 自死锁）。实现抢占前
    //   本临界区必须 irq-save：CpuImpl::disable_irq() / restore_irq()，
    //   决策注记见 core/src/irq.rs。
    let (from_ptr, to_ptr, next_owner): (
        *mut ContextImpl,
        *const ContextImpl,
        Option<ComponentId>,
    ) = {
        let mut cpu_guard = cpu().lock();
        let mut table = task::get_task_table().lock();

        let from_ptr: *mut ContextImpl = match from {
            Some(id) => {
                let after = after.ok_or(SchedError::InvalidTransition)?;
                table
                    .transition(id, after)
                    .map_err(|_| SchedError::InvalidTransition)?;
                let rec = table.get_mut(id).ok_or(SchedError::NotFound)?;
                rec.context.as_mut() as *mut ContextImpl
            }
            None => {
                if cpu_guard.anchor.is_none() {
                    cpu_guard.anchor = Some(Box::new(CpuImpl::new_context(0, 0)));
                }
                cpu_guard
                    .anchor
                    .as_mut()
                    .expect("anchor just ensured")
                    .as_mut() as *mut ContextImpl
            }
        };

        let (to_ptr, next_owner): (*const ContextImpl, Option<ComponentId>) = match next {
            Some(id) => {
                // owner 先取（Copy），再可变借 table 推进状态。
                let owner = table.get(id).ok_or(SchedError::NotFound)?.owner();
                table
                    .transition(id, TaskState::Running(CpuId(0)))
                    .map_err(|_| SchedError::InvalidTransition)?;
                cpu_guard.current = Some(id);
                let rec = table.get(id).ok_or(SchedError::NotFound)?;
                (rec.context.as_ref() as *const ContextImpl, Some(owner))
            }
            None => {
                cpu_guard.current = None;
                (
                    cpu_guard
                        .anchor
                        .as_ref()
                        .expect("anchor exists after first run")
                        .as_ref() as *const ContextImpl,
                    None,
                )
            }
        };
        (from_ptr, to_ptr, next_owner)
    }; // 全部锁在此释放

    drop(guard);

    // Phase 2：锁外 revoke（仅 abort 路径）+ 按 incoming 安装逃逸 guard + 切换。
    if let Some((dead, owner)) = abort {
        crate::component::fail_component(owner, ComponentLoadError::TaskPanicked(dead));
    }
    // Trace：状态 commit 已完成，这里记录"要切给谁"。放在真正切走之前，因此
    // abort 路径上的顺序是 … → ComponentState{Failed} → TaskSwitch（先落账再切走）。
    if let Some(id) = next {
        crate::trace::emit(crate::trace::TraceEvent::TaskSwitch { from, to: id });
    }
    match next {
        Some(id) => containment::enter_task(id, next_owner.expect("task owner is known")),
        None => containment::enter_anchor(),
    }

    // 单 CPU 协作式：此处无并发、无 yield 点。
    // SAFETY: 两个指针分别指向任务记录的 Box（堆地址稳定）与锚点 Box
    // （全局静态内，地址稳定）；to 侧上下文由 new_context 或上一次切换保存。
    unsafe {
        CpuImpl::context_switch(&mut *from_ptr, &*to_ptr);
    }
    Ok(())
}

/// 任务 panic 收尾：在 Core-owned abort 上下文里把已死任务 commit 为
/// `Exited`、撤销失败组件的 authority，并重新调度。
///
/// 由 [`crate::component::containment`] 的 task-abort trampoline 调用——那里
/// 是干净的 Core 上下文（不在死任务的栈上、不持任何锁）。本函数**永不返回**
/// 到 panic 的任务：`schedule_next` 把 abort 上下文保存进死任务的 context
/// box 后切走，而死任务不会再被调度。若 Core 不变式被破坏（例如任务并非
/// `Running`）则停机，绝不恢复不可信的上下文。
pub(crate) fn abort_current_task(task: TaskId, owner: ComponentId) -> ! {
    let _ = schedule_next(Some(task), Some(TaskState::Exited), Some((task, owner)));
    loop {
        core::hint::spin_loop();
    }
}

/// 从"任务之外"（组件 init / monitor 调用栈）进入调度：把所有 Runnable
/// 任务轮流跑到尽。没有 Runnable 任务时直接返回（no-op）。
/// 全部任务退出（或阻塞）后，控制权在锚点上下文回到调用者。
pub fn run() -> Result<(), SchedError> {
    deny_in_irq_context()?;
    if collect_runnable().is_empty() {
        return Ok(());
    }
    schedule_next(None, None, None)
}

/// 当前任务主动让出 CPU：Running → Runnable，切换走。再次被选中时返回。
pub fn yield_current() -> Result<(), SchedError> {
    deny_in_irq_context()?;
    let current = cpu().lock().current.ok_or(SchedError::NoCurrent)?;
    schedule_next(Some(current), Some(TaskState::Runnable), None)
}

/// 当前任务退出：Running → Exited，切换走。**本任务从此不再恢复**——
/// 若还有 Runnable 任务则它们接管；全部退出后控制权回到锚点。
pub fn exit_current() -> Result<(), SchedError> {
    deny_in_irq_context()?;
    let current = cpu().lock().current.ok_or(SchedError::NoCurrent)?;
    schedule_next(Some(current), Some(TaskState::Exited), None)
}

/// 时钟抢占入口（`timer::on_trap` 调用；中断上下文）。
///
/// # 设计决策（TODO，选型 + 实现留给人）
///
/// - **延迟重调度**：只置"需要重调度"标志，安全点消费（实现简单；抢占延迟
///   一个安全点，安全点的选择本身是设计点）；
/// - **trap 内直接切换**：处理器里直接走 `schedule_next`——进入前必须先解决
///   两件事：(1) phase-1 临界区 irq-save（见下）；(2) `__switch` 的 sstatus
///   语义（trap 上下文 SIE=0，切出去的目标恢复后由谁开中断要想清楚；
///   协作式模型下 sstatus 不在 RiscvContext 里，这正是需要正面回答的地方）。
pub fn on_timer_tick() {
    todo!("C5: 时钟抢占")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::image::ComponentImageId;
    use crate::component::interface::InterfaceRegistry;
    use crate::component::registry::Registry;
    use alloc::vec;
    use core::ptr;

    /// 测试用镜像身份：registry 只把它当身份键（image 表是另一份真相）。
    const IMAGE: ComponentImageId = ComponentImageId::from_raw(1);

    /// 串行化触碰进程全局 task table / registry 的调度测试。
    static SCHED_TEST_LOCK: spin::Mutex<()> = spin::Mutex::new(());

    const ENTRY: usize = 0x8000_0000;

    /// 全局 CPU 真相是进程级 `Once`：`run()` / `yield` / `exit` 会留下
    /// `current` / `anchor`，用例结束必须复位，否则污染后续用例（例如 handle
    /// 的 ambient 解析依赖 `current_task() == None`，见 `handle/context.rs`）。
    fn reset_cpu() {
        let mut state = cpu().lock();
        state.anchor = None;
        state.current = None;
    }

    fn set_current(id: Option<TaskId>) {
        cpu().lock().current = id;
    }

    /// 全局 registry 里的一个 `Ready` 活实例（同一 image 可无限复用，id 跨用例累积）。
    fn ready_component(_name: &[u8]) -> ComponentId {
        let mut reg = registry::get_registry().lock();
        let id = reg.declare(IMAGE).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    /// 全局 task 表里的一个 `Runnable` 任务（owner 是否存活由调用方决定）。
    fn runnable_task(owner: ComponentId) -> TaskId {
        let task = crate::task::get_task_table()
            .lock()
            .create(owner, ENTRY, ptr::null_mut())
            .unwrap();
        crate::task::get_task_table()
            .lock()
            .transition(task, TaskState::Runnable)
            .unwrap();
        task
    }

    /// 向全局 interfaces 发布一个 `scheduler` policy（provider 走到 `Ready`）。
    ///
    /// `vtable` 只以指针存入 binding，调用方的局部 vtable 必须活到用例结束。
    fn publish_policy(_name: &[u8], vtable: &SchedulerPolicyApi) -> ComponentId {
        let provider = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(IMAGE).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };
        {
            let reg = registry::get_registry().lock();
            let mut ifs = crate::component::interface::get_interfaces().lock();
            ifs.stage_publish(
                &reg,
                provider,
                b"scheduler",
                InterfaceKind::Policy,
                SCHEDULER_POLICY_ABI,
                vtable as *const SchedulerPolicyApi as *const (),
                ptr::null_mut(),
            )
            .unwrap();
            ifs.commit_pending(&reg, provider).unwrap();
        }
        registry::get_registry()
            .lock()
            .finish_start(provider)
            .unwrap();
        provider
    }

    /// 隔离一个不再可信的 policy provider：`bind` 的存活复验从此失败
    /// （`resolve_policy` → `NoPolicy`）。让"无 policy"用例与执行顺序无关。
    fn retire_policy(provider: ComponentId) {
        registry::get_registry().lock().mark_failed(provider).ok();
    }

    fn state_of(task: TaskId) -> TaskState {
        crate::task::get_task_table()
            .lock()
            .get(task)
            .expect("task record")
            .state()
    }

    fn remove_task(task: TaskId) {
        assert!(crate::task::get_task_table().lock().remove(task).is_ok());
    }

    /// 纯逻辑：任务耗尽后 run() 不再切换（无锚点捕获、无 state 变更）。
    #[test]
    fn run_with_no_runnable_tasks_is_noop() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _guard = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        // 空表：run 直接返回，不 panic、不切换。
        assert_eq!(run(), Ok(()));
    }

    /// `Failed` 组件拥有的 Runnable 任务既不进入候选，也不通过 commit 门禁；
    /// `run()` 安全返回 no-op（不挂起、不误调度）。
    #[test]
    fn failed_component_tasks_are_not_scheduled() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();

        // Given：一个 Ready 组件 + 一个 Runnable 任务（直接进全局 task 表）。
        let owner = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(IMAGE).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        let task = crate::task::get_task_table()
            .lock()
            .create(owner, 0x8000_0000, ptr::null_mut())
            .unwrap();
        crate::task::get_task_table()
            .lock()
            .transition(task, TaskState::Runnable)
            .unwrap();

        // 活实例：候选包含它，commit 门禁放行。
        assert!(collect_runnable().contains(&task));
        assert!(owner_still_runnable(task));

        // When：组件失败。
        registry::get_registry().lock().mark_failed(owner).unwrap();

        // Then：候选剔除、commit 门禁拒绝、run() no-op。
        assert!(!collect_runnable().contains(&task));
        assert!(!owner_still_runnable(task));
        assert_eq!(run(), Ok(()));

        // 清理：移除任务，避免污染其它调度测试。
        assert!(crate::task::get_task_table().lock().remove(task).is_ok());
    }

    /// 提议验证：不在 runnable 列表里的 id 一律拒绝——回退到 id 序首项，
    /// 且 provider 被标 Failed（隔离错误组件，Core 不被挂起）。
    #[test]
    fn invalid_proposal_falls_back_and_isolates_provider() {
        crate::memory::test_support::ensure_init();
        let _guard = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();

        // 直接构造 registry + interfaces（不经过全局）：host 可测的生产类型。
        let mut reg = Registry::new();
        let mut ifs = InterfaceRegistry::new();

        // 两个 Runnable 任务（id 0、1）。
        let mut table = crate::task::TaskTable::new();
        const ENTRY: usize = 0x8000_0000;
        let owner = ComponentId::from_raw(1);
        let a = table.create(owner, ENTRY, ptr::null_mut()).unwrap();
        let b = table.create(owner, ENTRY, ptr::null_mut()).unwrap();
        table.transition(a, TaskState::Runnable).unwrap();
        table.transition(b, TaskState::Runnable).unwrap();

        // 一个 Ready 的"调度器"组件，发布坏策略（提议 999，不在列表里）。
        extern "C" fn bad_choose(
            _ctx: *mut (),
            _runnable: *const u32,
            _count: usize,
            _current: u32,
        ) -> u32 {
            999
        }
        let vtable = SchedulerPolicyApi {
            choose_next: bad_choose,
        };
        let provider = reg.declare(IMAGE).unwrap();
        reg.resolve(provider).unwrap();
        reg.begin_start(provider).unwrap();
        ifs.stage_publish(
            &reg,
            provider,
            b"scheduler",
            InterfaceKind::Policy,
            SCHEDULER_POLICY_ABI,
            &vtable as *const SchedulerPolicyApi as *const (),
            ptr::null_mut(),
        )
        .unwrap();
        ifs.commit_pending(&reg, provider).unwrap();
        reg.finish_start(provider).unwrap();

        // 走 resolve_policy 的局部版本：直接对局部 registry 解析（不碰全局）。
        let view = ifs
            .bind(
                &reg,
                b"scheduler",
                InterfaceKind::Policy,
                SCHEDULER_POLICY_ABI,
            )
            .unwrap();
        let vtable = unsafe { &*(view.api as *const SchedulerPolicyApi) };
        let ids: Vec<u32> = vec![a.raw(), b.raw()];
        let proposed = (vtable.choose_next)(view.ctx, ids.as_ptr(), ids.len(), u32::MAX);
        let proposed = TaskId::from_raw(proposed);
        assert_eq!(
            proposed,
            TaskId::from_raw(999),
            "坏策略确实提议了不存在的任务"
        );

        // Core 侧验证逻辑：非法提议 → 回退 + 隔离（等价于 pick_next 的内部路径）。
        assert!(![a, b].contains(&proposed));
        reg.mark_failed(provider).unwrap();
        let fallback = vec![a, b][0];
        assert_eq!(fallback, a, "回退 = id 序首项（确定性）");
        assert_eq!(
            reg.get(provider).unwrap().state,
            crate::component::ComponentState::Failed
        );
    }

    /// 对抗（**真实全局路径**，非局部复刻）：坏调度器提议不存在的任务。
    ///
    /// 断言三件事：
    /// 1. Core 真相没有被错误组件改写 —— 退回确定性回退（id 序首项），
    ///    绝不把 CPU 交给那个不存在的 TaskId(999)；
    /// 2. 错误 provider 被隔离（`Failed`）；
    /// 3. **真实事件序列**（只看 provider 自己的事件，因此与并行测试互不干扰）：
    ///    `出生 → Ready → 坏提议 → Core 拒绝 → 隔离`。
    #[test]
    #[cfg(feature = "trace")]
    fn invalid_proposal_emits_real_event_sequence_and_keeps_truth() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _trace = crate::trace::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();

        // Given：一个 Ready 的坏调度器组件 —— 永远提议 TaskId(999)。
        extern "C" fn bad_choose(_: *mut (), _: *const u32, _: usize, _: u32) -> u32 {
            999
        }
        let vtable = SchedulerPolicyApi {
            choose_next: bad_choose,
        };
        let provider = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(IMAGE).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };
        {
            let reg = registry::get_registry().lock();
            let mut ifs = crate::component::interface::get_interfaces().lock();
            ifs.stage_publish(
                &reg,
                provider,
                b"scheduler",
                InterfaceKind::Policy,
                SCHEDULER_POLICY_ABI,
                &vtable as *const SchedulerPolicyApi as *const (),
                ptr::null_mut(),
            )
            .unwrap();
            ifs.commit_pending(&reg, provider).unwrap();
        }
        registry::get_registry()
            .lock()
            .finish_start(provider)
            .unwrap();

        // 一个 Ready 的 task owner + 一个 Runnable 任务（真实全局 task 表）。
        let owner = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(IMAGE).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        let task = crate::task::get_task_table()
            .lock()
            .create(owner, 0x8000_0000, ptr::null_mut())
            .unwrap();
        crate::task::get_task_table()
            .lock()
            .transition(task, TaskState::Runnable)
            .unwrap();

        // When：走**真实** pick_next（resolve_policy → bind → 提议 → Core 验证）。
        let runnable = collect_runnable();
        assert!(runnable.contains(&task), "活实例的任务应在候选里");
        let picked = pick_next(&runnable).unwrap();

        // Then 1：真相未被改写 —— 回退到 id 序首项，而不是那个不存在的 999。
        assert_ne!(picked, Some(TaskId::from_raw(999)));
        assert_eq!(
            picked,
            runnable.first().copied(),
            "回退 = id 序首项（确定性）"
        );

        // Then 2：错误 provider 被隔离。
        assert_eq!(
            registry::get_registry().lock().get(provider).unwrap().state,
            crate::component::ComponentState::Failed
        );

        // Then 3：真实事件序列（子序列匹配，对并行测试插入的事件免疫）。
        use crate::component::ComponentState;
        use crate::trace::{RejectReason, TraceEvent};
        crate::trace::test_support::assert_subsequence(
            &[
                TraceEvent::ComponentState {
                    component: provider,
                    from: None,
                    to: ComponentState::Declared,
                },
                TraceEvent::ComponentState {
                    component: provider,
                    from: Some(ComponentState::Declared),
                    to: ComponentState::Resolved,
                },
                TraceEvent::ComponentState {
                    component: provider,
                    from: Some(ComponentState::Resolved),
                    to: ComponentState::Starting,
                },
                TraceEvent::ComponentState {
                    component: provider,
                    from: Some(ComponentState::Starting),
                    to: ComponentState::Ready,
                },
                TraceEvent::PolicyProposal {
                    component: provider,
                    task: TaskId::from_raw(999),
                },
                TraceEvent::PolicyRejected {
                    component: provider,
                    reason: RejectReason::NotRunnable,
                },
                TraceEvent::ComponentState {
                    component: provider,
                    from: Some(ComponentState::Ready),
                    to: ComponentState::Failed,
                },
            ],
            &crate::trace::test_support::events(),
        );

        // 清理：移除任务，避免污染其它调度测试。
        assert!(crate::task::get_task_table().lock().remove(task).is_ok());
    }

    /// 性能基线（`make bench`）：**调度 proposal + Core 验证** 的成本。
    ///
    /// 只测到 `pick_next` 为止。commit（`TaskTable::transition`）单独测；
    /// 真正的 context switch 必须在目标端测 —— host 的 `context_switch` 是
    /// Fake no-op（见 docs/benchmark.md §6）。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_scheduler_propose_and_validate() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();

        // 好调度器：永远提议 runnable[0]（合法 → 走 accept 路径）。
        extern "C" fn good_choose(_: *mut (), runnable: *const u32, count: usize, _: u32) -> u32 {
            if count == 0 {
                u32::MAX
            } else {
                unsafe { *runnable }
            }
        }
        let vtable = SchedulerPolicyApi {
            choose_next: good_choose,
        };
        let provider = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(IMAGE).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };
        {
            let reg = registry::get_registry().lock();
            let mut ifs = crate::component::interface::get_interfaces().lock();
            ifs.stage_publish(
                &reg,
                provider,
                b"scheduler",
                InterfaceKind::Policy,
                SCHEDULER_POLICY_ABI,
                &vtable as *const SchedulerPolicyApi as *const (),
                ptr::null_mut(),
            )
            .unwrap();
            ifs.commit_pending(&reg, provider).unwrap();
        }
        registry::get_registry()
            .lock()
            .finish_start(provider)
            .unwrap();

        let owner = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(IMAGE).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        let task = crate::task::get_task_table()
            .lock()
            .create(owner, 0x8000_0000, ptr::null_mut())
            .unwrap();
        crate::task::get_task_table()
            .lock()
            .transition(task, TaskState::Runnable)
            .unwrap();
        let runnable = collect_runnable();

        crate::bench::report_environment();

        // 全路径：resolve_policy（锁 + bind）+ 提议 + Core 验证。
        crate::bench::run("sched.pick_next", 1_000, || pick_next(&runnable).unwrap()).report();

        // commit：状态转移的验证 + 落笔（不含真正切换）。
        let mut table = crate::task::TaskTable::new();
        let local_owner = ComponentId::from_raw(0x7b);
        let local = table
            .create(local_owner, 0x8000_0000, ptr::null_mut())
            .unwrap();
        table.transition(local, TaskState::Runnable).unwrap();
        let mut commit = crate::bench::Bench::new("sched.task_transition");
        commit.run(1_000, || {
            table
                .transition(local, TaskState::Running(CpuId(0)))
                .unwrap();
            table.transition(local, TaskState::Runnable).unwrap();
        });
        commit.finish().report();

        // 清理：移除任务，避免污染其它调度测试。
        assert!(crate::task::get_task_table().lock().remove(task).is_ok());
    }

    /// 对抗：提议一个**存在但不可运行**的任务（`Created`，不在 Core 裁剪过的
    /// runnable 列表里）——与"提议不存在的 id"同等拒绝：Core 回退 id 序首项、
    /// 隔离坏 provider、发 `NotRunnable` 事件；被提议任务的状态不被改写。
    #[test]
    #[cfg(feature = "trace")]
    fn proposal_of_existing_but_not_runnable_task_is_rejected() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _trace = crate::trace::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();

        // Given：一个 Created 任务（存在但不在候选里）+ 一个 Runnable 任务。
        let owner = ready_component(b"sched_created_owner");
        let created = crate::task::get_task_table()
            .lock()
            .create(owner, ENTRY, ptr::null_mut())
            .unwrap();
        let live = runnable_task(owner);
        assert_eq!(state_of(created), TaskState::Created);

        // Given：一个坏策略，永远提议那个 Created 任务。
        use core::sync::atomic::{AtomicU32, Ordering};
        static PROPOSED: AtomicU32 = AtomicU32::new(0);
        PROPOSED.store(created.raw(), Ordering::SeqCst);
        extern "C" fn propose_created(_: *mut (), _: *const u32, _: usize, _: u32) -> u32 {
            PROPOSED.load(Ordering::SeqCst)
        }
        let vtable = SchedulerPolicyApi {
            choose_next: propose_created,
        };
        let provider = publish_policy(b"sched_created_policy", &vtable);

        // When：走真实提议验证路径（Core 自己裁剪候选 → 验证 → 拒绝）。
        let runnable = collect_runnable();
        assert!(runnable.contains(&live), "活任务应在候选里");
        assert!(!runnable.contains(&created), "Created 不进候选");
        let picked = pick_next(&runnable).unwrap();

        // Then：拒绝 + 确定性回退；被提议任务状态不变。
        assert_eq!(picked, Some(live));
        assert_eq!(
            state_of(created),
            TaskState::Created,
            "被拒绝的提议不得改写真相"
        );
        assert_eq!(
            registry::get_registry().lock().get(provider).unwrap().state,
            crate::component::ComponentState::Failed,
            "坏 provider 被隔离"
        );

        // Then：真实事件序列：坏提议 → Core 拒绝 → provider 隔离。
        use crate::component::ComponentState;
        use crate::trace::{RejectReason, TraceEvent};
        crate::trace::test_support::assert_subsequence(
            &[
                TraceEvent::PolicyProposal {
                    component: provider,
                    task: created,
                },
                TraceEvent::PolicyRejected {
                    component: provider,
                    reason: RejectReason::NotRunnable,
                },
                TraceEvent::ComponentState {
                    component: provider,
                    from: Some(ComponentState::Ready),
                    to: ComponentState::Failed,
                },
            ],
            &crate::trace::test_support::events(),
        );

        // 清理。
        remove_task(created);
        remove_task(live);
        retire_policy(provider);
    }

    /// 对抗（**真实 commit 门禁**）：scheduler provider 恰好是唯一 Runnable
    /// 任务的 owner。坏提议触发 provider 隔离，随后回退任务过不了 commit-time
    /// `owner_still_runnable` 复验 —— Core 退回锚点，绝不把 CPU 交给已死实例
    /// 的任务；全程不挂起、不改写任务状态。
    ///
    /// 记录一个契约缺口：`RejectReason::OwnerNotRunnable` 目前是**未发射**的
    /// 词汇（Commit 门禁静默回退），本用例显式断言它没有出现——未来该门禁若
    /// 开始发事件，这条断言会失败并提醒更新事件契约。
    #[test]
    #[cfg(feature = "trace")]
    fn dead_owner_gate_blocks_dispatch_of_isolated_providers_task() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _trace = crate::trace::test_support::GUARD.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：同一实例既是 policy provider，又是唯一 Runnable 任务的 owner。
        extern "C" fn propose_ghost(_: *mut (), _: *const u32, _: usize, _: u32) -> u32 {
            0xDEAD
        }
        let vtable = SchedulerPolicyApi {
            choose_next: propose_ghost,
        };
        let provider = publish_policy(b"sched_owner_gate", &vtable);
        let task = runnable_task(provider);
        assert!(
            collect_runnable().contains(&task),
            "活 owner 的任务应在候选里"
        );

        // When：从锚点进入调度（真实 run → pick_next → commit 门禁）。
        let result = run();

        // Then 1：不挂起、不 commit —— 任务保持 Runnable，CPU 无 current。
        assert_eq!(result, Ok(()));
        assert_eq!(
            state_of(task),
            TaskState::Runnable,
            "已死 owner 的任务不得被 dispatch"
        );
        assert_eq!(current_task(), None);

        // Then 2：坏提议被拒绝、provider 被隔离。
        assert_eq!(
            registry::get_registry().lock().get(provider).unwrap().state,
            crate::component::ComponentState::Failed
        );
        use crate::component::ComponentState;
        use crate::trace::{RejectReason, TraceEvent};
        let events = crate::trace::test_support::events();
        crate::trace::test_support::assert_subsequence(
            &[
                TraceEvent::PolicyProposal {
                    component: provider,
                    task: TaskId::from_raw(0xDEAD),
                },
                TraceEvent::PolicyRejected {
                    component: provider,
                    reason: RejectReason::NotRunnable,
                },
                TraceEvent::ComponentState {
                    component: provider,
                    from: Some(ComponentState::Ready),
                    to: ComponentState::Failed,
                },
            ],
            &events,
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, TraceEvent::TaskSwitch { to, .. } if *to == task)),
            "Core 不得把 CPU 交给已死实例的任务（无 TaskSwitch）"
        );
        assert!(
            !events.iter().any(|event| matches!(
                event,
                TraceEvent::PolicyRejected {
                    component,
                    reason: RejectReason::OwnerNotRunnable,
                } if *component == provider
            )),
            "commit 门禁当前是静默回退，不发射 OwnerNotRunnable"
        );

        // 清理。
        remove_task(task);
        retire_policy(provider);
        reset_cpu();
    }

    /// 未绑定 SchedulerPolicy 时 Core 不猜、不退化成内置调度器：`run()` 返回
    /// `NoPolicy`，任务保持 Runnable、无 current、无状态推进。
    #[test]
    fn dispatch_without_policy_is_rejected_and_changes_nothing() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：活 owner + Runnable 任务，全程没有发布任何 policy。
        let owner = ready_component(b"sched_no_policy_owner");
        let task = runnable_task(owner);
        assert!(collect_runnable().contains(&task));

        // When
        let result = run();

        // Then
        assert_eq!(result, Err(SchedError::NoPolicy));
        assert_eq!(
            state_of(task),
            TaskState::Runnable,
            "无 policy 不得推进任务状态"
        );
        assert_eq!(current_task(), None);

        // 清理。
        remove_task(task);
        reset_cpu();
    }

    /// provider 已死（Failed）的 policy 等价于没有 policy：解析在 bind 的存活
    /// 复验处失败 → `NoPolicy`；Core 不会静默换用其它 provider。
    #[test]
    fn failed_policy_provider_resolves_to_no_policy() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：一个发布过 policy 的 provider + 一个无关的活 owner 与任务。
        extern "C" fn propose_ghost(_: *mut (), _: *const u32, _: usize, _: u32) -> u32 {
            0xDEAD
        }
        let vtable = SchedulerPolicyApi {
            choose_next: propose_ghost,
        };
        let provider = publish_policy(b"sched_dead_policy", &vtable);
        let owner = ready_component(b"sched_dead_policy_owner");
        let task = runnable_task(owner);

        // When：provider 在调度请求之前死亡（隔离）。
        retire_policy(provider);
        let result = run();

        // Then：binding 的存活复验失败 = 没有可用 policy。
        assert_eq!(result, Err(SchedError::NoPolicy));
        assert_eq!(state_of(task), TaskState::Runnable);
        assert_eq!(current_task(), None);

        // 清理。
        remove_task(task);
        reset_cpu();
    }

    /// `yield` / `exit` 只属于正在运行的任务：本 CPU 无 current 时两个入口都
    /// 返回 `NoCurrent`，且不产生任何状态/上下文副作用。
    #[test]
    fn yield_and_exit_without_current_task_are_rejected() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::task::init();
        init();
        reset_cpu();

        assert_eq!(current_task(), None);
        assert_eq!(yield_current(), Err(SchedError::NoCurrent));
        assert_eq!(exit_current(), Err(SchedError::NoCurrent));
        assert_eq!(current_task(), None);
    }

    /// 上下文种类门禁：IRQ 回调作用域内 `run` / `yield` / `exit` 一律拒绝
    /// （`-EINVAL`，ABI 上是负 errno），绝不 panic、绝不切换任务；离开作用域后
    /// 被中断的上下文恢复，正常路径不受影响。
    #[test]
    fn scheduler_operations_are_rejected_in_irq_context() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        reset_cpu();
        containment::enter_anchor();

        containment::with_irq_scope(ComponentId::from_raw(0xBEEF), || {
            assert_eq!(run(), Err(SchedError::InvalidTransition));
            assert_eq!(yield_current(), Err(SchedError::InvalidTransition));
            assert_eq!(exit_current(), Err(SchedError::InvalidTransition));
            assert_eq!(
                crate::errno::Errno::from(SchedError::InvalidTransition).code(),
                -22,
                "ABI 上是负 errno（EINVAL），不是 panic"
            );
        });

        // 离开 scope：上下文种类恢复，不再走 IRQ 拒绝路径（其它用例可能留下
        // 无关 Runnable 任务，故只断言不再是 IRQ 拒绝）。
        assert!(!containment::in_irq_context());
        assert_ne!(run(), Err(SchedError::InvalidTransition));
        assert_eq!(current_task(), None);
        containment::enter_anchor();
    }

    /// 非法转换（yield）：current 指向一个已 `Exited` 的任务（陈旧 current，
    /// Core 不变式被破坏）。yield 只能把 `Running` 推回 `Runnable`，其余状态
    /// 一律 `InvalidTransition`；失败路径既不回滚也不推进真相。
    #[test]
    fn yield_of_exited_current_is_invalid_transition() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：一个已走完生命周期（Exited）的任务被错记为本 CPU 的 current。
        let owner = ready_component(b"sched_stale_exited_owner");
        let task = runnable_task(owner);
        crate::task::get_task_table()
            .lock()
            .transition(task, TaskState::Running(CpuId(0)))
            .unwrap();
        crate::task::get_task_table()
            .lock()
            .transition(task, TaskState::Exited)
            .unwrap();
        set_current(Some(task));

        // When
        let result = yield_current();

        // Then：状态机拒绝推进，当前记录保持不变。
        assert_eq!(result, Err(SchedError::InvalidTransition));
        assert_eq!(state_of(task), TaskState::Exited);
        assert_eq!(current_task(), Some(task), "失败路径不推进 current");

        // 清理。
        remove_task(task);
        reset_cpu();
    }

    /// 非法转换（exit）：`Created` 任务从未运行，不能被 exit 提交为 `Exited`。
    #[test]
    fn exit_of_created_current_is_invalid_transition() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：一个 Created 任务被错记为本 CPU 的 current。
        let owner = ready_component(b"sched_stale_created_owner");
        let task = crate::task::get_task_table()
            .lock()
            .create(owner, ENTRY, ptr::null_mut())
            .unwrap();
        set_current(Some(task));

        // When
        let result = exit_current();

        // Then：非法转换被拒绝，任务与 current 都不动。
        assert_eq!(result, Err(SchedError::InvalidTransition));
        assert_eq!(state_of(task), TaskState::Created);
        assert_eq!(current_task(), Some(task));

        // 清理。
        remove_task(task);
        reset_cpu();
    }

    /// 内部契约防御：`from = Some(id)` 必须显式携带目标状态（生产入口
    /// `run`/`yield`/`exit` 都携带）；缺失时 Core 拒绝推进而不是猜测——
    /// `InvalidTransition`，且 current / 任务状态都不动。
    #[test]
    fn advance_without_target_state_is_rejected() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：一个任务被记为本 CPU 的 current，但没有给出目标状态。
        let owner = ready_component(b"sched_no_after_owner");
        let task = crate::task::get_task_table()
            .lock()
            .create(owner, ENTRY, ptr::null_mut())
            .unwrap();
        set_current(Some(task));

        // When：缺失 `after`（私有入口防御；生产路径永不这样调用）。
        let result = schedule_next(Some(task), None, None);

        // Then：拒绝，真相不变。
        assert_eq!(result, Err(SchedError::InvalidTransition));
        assert_eq!(state_of(task), TaskState::Created);
        assert_eq!(current_task(), Some(task));

        // 清理。
        remove_task(task);
        reset_cpu();
    }

    /// Abort 交接（**bookkeeping 部分**；栈抛弃 / 永不返回是 QEMU 契约）：
    /// 任务 panic 后 Core 在同一次 commit 里把死任务标 `Exited`、选好后继、
    /// 再 `fail_component` 撤销 owner 的 authority——`ComponentState{Failed}`
    /// 事件先于 `TaskSwitch` 落账，Core 不被失败组件挂起。
    ///
    /// 直接调用私有 `schedule_next`：生产入口 `abort_current_task` 在 host 上
    /// 会落入永不返回的自旋（见该函数），真实 trampoline 由 QEMU ArchTest 覆盖。
    #[test]
    #[cfg(feature = "trace")]
    fn abort_handoff_commits_exit_fails_owner_and_switches_to_successor() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _trace = crate::trace::test_support::GUARD.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();
        crate::resource::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：活 owner 与活后继 owner；一个 Running 的"panicking"任务 +
        // 一个 Runnable 后继（后继必须在 owner 死亡前完成选择）。
        extern "C" fn first_runnable(
            _: *mut (),
            runnable: *const u32,
            count: usize,
            _: u32,
        ) -> u32 {
            if count == 0 {
                u32::MAX
            } else {
                unsafe { *runnable }
            }
        }
        let vtable = SchedulerPolicyApi {
            choose_next: first_runnable,
        };
        let provider = publish_policy(b"sched_abort_policy", &vtable);
        let owner = ready_component(b"sched_abort_owner");
        let succ_owner = ready_component(b"sched_abort_successor_owner");
        let dying = runnable_task(owner);
        crate::task::get_task_table()
            .lock()
            .transition(dying, TaskState::Running(CpuId(0)))
            .unwrap();
        let successor = runnable_task(succ_owner);
        set_current(Some(dying));

        // When：abort 交接（等价于 abort_current_task 里的 schedule_next 调用）。
        let result = schedule_next(Some(dying), Some(TaskState::Exited), Some((dying, owner)));

        // Then 1：死任务 Exited、后继 Running、current = 后继。
        assert_eq!(result, Ok(()));
        assert_eq!(state_of(dying), TaskState::Exited);
        assert_eq!(state_of(successor), TaskState::Running(CpuId(0)));
        assert_eq!(current_task(), Some(successor));

        // Then 2：owner 逻辑死亡；后继 owner 不受影响。
        assert_eq!(
            registry::get_registry().lock().get(owner).unwrap().state,
            crate::component::ComponentState::Failed
        );
        assert_eq!(
            registry::get_registry()
                .lock()
                .get(succ_owner)
                .unwrap()
                .state,
            crate::component::ComponentState::Ready
        );

        // Then 3：事件顺序——先落 owner 的 Failed 账，再 TaskSwitch。
        use crate::component::ComponentState;
        use crate::trace::TraceEvent;
        crate::trace::test_support::assert_subsequence(
            &[
                TraceEvent::ComponentState {
                    component: owner,
                    from: Some(ComponentState::Ready),
                    to: ComponentState::Failed,
                },
                TraceEvent::TaskSwitch {
                    from: Some(dying),
                    to: successor,
                },
            ],
            &crate::trace::test_support::events(),
        );

        // 清理：本路径没有经过锚点进入（from = Some），anchor 未建立——
        // 只复位边界与 CPU 真相，不经过 exit 路径。
        containment::enter_anchor();
        remove_task(dying);
        remove_task(successor);
        retire_policy(provider);
        reset_cpu();
    }

    /// 完整接受链（host 侧，`context_switch` 为 no-op）：run → yield → exit ×2。
    ///
    /// 断言三件事：
    /// 1. commit 真相：任务状态与 `current_task()` 的每次推进；
    /// 2. policy 输入契约：裁剪后的 runnable 数量与 current 参数按 Core 真相传入；
    /// 3. 真实事件序列 `PolicyProposal → PolicyAccepted → TaskSwitch`（子序列匹配）。
    #[test]
    #[cfg(feature = "trace")]
    fn run_yield_exit_commit_sequence_is_observable_in_truth_and_trace() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _trace = crate::trace::test_support::GUARD.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：好策略（提议 id 序首项）+ 一个活 owner + 两个 Runnable 任务。
        use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
        static SEEN_CURRENT: AtomicU32 = AtomicU32::new(u32::MAX);
        static SEEN_COUNT: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn first_runnable(
            _: *mut (),
            runnable: *const u32,
            count: usize,
            current: u32,
        ) -> u32 {
            SEEN_CURRENT.store(current, Ordering::SeqCst);
            SEEN_COUNT.store(count, Ordering::SeqCst);
            if count == 0 {
                u32::MAX
            } else {
                unsafe { *runnable }
            }
        }
        let vtable = SchedulerPolicyApi {
            choose_next: first_runnable,
        };
        let provider = publish_policy(b"sched_commit_policy", &vtable);
        let owner = ready_component(b"sched_commit_owner");
        let a = runnable_task(owner);
        let b = runnable_task(owner);
        assert!(a.raw() < b.raw(), "BTreeMap 迭代序 = id 升序");

        // When 1：锚点 → 调度。A 拿到 CPU（id 序首项）。
        assert_eq!(run(), Ok(()));
        assert_eq!(
            SEEN_COUNT.load(Ordering::SeqCst),
            2,
            "policy 只看到两个活任务"
        );
        assert_eq!(
            SEEN_CURRENT.load(Ordering::SeqCst),
            u32::MAX,
            "从锚点进入时无 current"
        );
        assert_eq!(state_of(a), TaskState::Running(CpuId(0)));
        assert_eq!(state_of(b), TaskState::Runnable);
        assert_eq!(current_task(), Some(a));

        // When 2：A 让出 → 只剩 B 是候选；policy 看到的 current 是 A。
        assert_eq!(yield_current(), Ok(()));
        assert_eq!(SEEN_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(SEEN_CURRENT.load(Ordering::SeqCst), a.raw());
        assert_eq!(state_of(a), TaskState::Runnable);
        assert_eq!(state_of(b), TaskState::Running(CpuId(0)));
        assert_eq!(current_task(), Some(b));

        // When 3：B 退出 → A 接管（Runnable 里还有 A）。
        assert_eq!(exit_current(), Ok(()));
        assert_eq!(SEEN_CURRENT.load(Ordering::SeqCst), b.raw());
        assert_eq!(state_of(b), TaskState::Exited);
        assert_eq!(state_of(a), TaskState::Running(CpuId(0)));
        assert_eq!(current_task(), Some(a));

        // When 4：A 退出 → 候选为空，policy 不再被咨询，控制权回锚点。
        assert_eq!(exit_current(), Ok(()));
        assert_eq!(
            SEEN_CURRENT.load(Ordering::SeqCst),
            b.raw(),
            "无候选时不咨询 policy"
        );
        assert_eq!(state_of(a), TaskState::Exited);
        assert_eq!(current_task(), None);

        // Then：真实事件序列（thread-local ring 只含本用例事件）。
        use crate::trace::TraceEvent;
        crate::trace::test_support::assert_subsequence(
            &[
                TraceEvent::PolicyProposal {
                    component: provider,
                    task: a,
                },
                TraceEvent::PolicyAccepted {
                    component: provider,
                    task: a,
                },
                TraceEvent::TaskSwitch { from: None, to: a },
                TraceEvent::PolicyProposal {
                    component: provider,
                    task: b,
                },
                TraceEvent::PolicyAccepted {
                    component: provider,
                    task: b,
                },
                TraceEvent::TaskSwitch {
                    from: Some(a),
                    to: b,
                },
                TraceEvent::PolicyProposal {
                    component: provider,
                    task: a,
                },
                TraceEvent::PolicyAccepted {
                    component: provider,
                    task: a,
                },
                TraceEvent::TaskSwitch {
                    from: Some(b),
                    to: a,
                },
            ],
            &crate::trace::test_support::events(),
        );

        // 清理。
        remove_task(a);
        remove_task(b);
        retire_policy(provider);
        reset_cpu();
    }

    /// 候选裁剪只放行活实例：死 owner 的 Runnable 任务既不进 policy 输入，
    /// 也不会被 commit；同一时刻活 owner 的任务照常被调度。
    #[test]
    fn dispatch_skips_dead_owner_and_runs_live_owner_task() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();
        crate::component::interface::init();
        reset_cpu();
        containment::enter_anchor();

        // Given：一个活 owner 的任务 + 一个 Failed owner 的任务；policy 提议首项。
        use core::sync::atomic::{AtomicUsize, Ordering};
        static SEEN_COUNT: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn count_and_first(
            _: *mut (),
            runnable: *const u32,
            count: usize,
            _: u32,
        ) -> u32 {
            SEEN_COUNT.store(count, Ordering::SeqCst);
            if count == 0 {
                u32::MAX
            } else {
                unsafe { *runnable }
            }
        }
        let vtable = SchedulerPolicyApi {
            choose_next: count_and_first,
        };
        let provider = publish_policy(b"sched_mixed_policy", &vtable);
        let live_owner = ready_component(b"sched_mixed_live_owner");
        let dead_owner = ready_component(b"sched_mixed_dead_owner");
        let live = runnable_task(live_owner);
        let dead = runnable_task(dead_owner);
        registry::get_registry()
            .lock()
            .mark_failed(dead_owner)
            .unwrap();

        // When
        assert_eq!(run(), Ok(()));

        // Then：policy 只看到活任务（1 个），被 dispatch 的也是它；死任务不动。
        assert_eq!(
            SEEN_COUNT.load(Ordering::SeqCst),
            1,
            "死实例任务不得进入 policy 输入"
        );
        assert_eq!(state_of(live), TaskState::Running(CpuId(0)));
        assert_eq!(state_of(dead), TaskState::Runnable, "死实例任务不得运行");
        assert_eq!(current_task(), Some(live));
        assert!(!collect_runnable().contains(&dead));

        // 清理：让 live 退出（无候选时回锚点），再摘除两个任务。
        assert_eq!(exit_current(), Ok(()));
        remove_task(live);
        remove_task(dead);
        retire_policy(provider);
        reset_cpu();
    }

    /// commit 门禁对"表里不存在的任务"必须 fail-closed：未知 id 不能被当作
    /// 可运行（`None` 不是 `Some`——不会误调度幽灵任务，也不会 panic）。
    #[test]
    fn commit_gate_fails_closed_for_unknown_task() {
        let _sched = SCHED_TEST_LOCK.lock();
        crate::task::init();
        init();
        crate::component::registry::init();

        assert!(!owner_still_runnable(TaskId::from_raw(0x0BAD_F00D)));
    }
}
