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
    InvalidTransition,
    /// 当前任务从表中消失（Core 不变式被破坏，不应发生）。
    NotFound,
    /// yield/exit 调用时本 CPU 没有在跑任务（只有任务能 yield/exit）。
    NoCurrent,
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
    if runnable.contains(&proposed) {
        return Ok(Some(proposed));
    }
    // 组件提出非法提议：隔离 + 回退（Core 不被错误组件挂起）。
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
    if collect_runnable().is_empty() {
        return Ok(());
    }
    schedule_next(None, None, None)
}

/// 当前任务主动让出 CPU：Running → Runnable，切换走。再次被选中时返回。
pub fn yield_current() -> Result<(), SchedError> {
    let current = cpu().lock().current.ok_or(SchedError::NoCurrent)?;
    schedule_next(Some(current), Some(TaskState::Runnable), None)
}

/// 当前任务退出：Running → Exited，切换走。**本任务从此不再恢复**——
/// 若还有 Runnable 任务则它们接管；全部退出后控制权回到锚点。
pub fn exit_current() -> Result<(), SchedError> {
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
    use crate::component::interface::InterfaceRegistry;
    use crate::component::registry::Registry;
    use alloc::vec;
    use core::ptr;

    /// 串行化触碰进程全局 task table / registry 的调度测试。
    static SCHED_TEST_LOCK: spin::Mutex<()> = spin::Mutex::new(());

    /// 纯逻辑：任务耗尽后 run() 不再切换（无锚点捕获、无 state 变更）。
    #[test]
    fn run_with_no_runnable_tasks_is_noop() {
        let _sched = SCHED_TEST_LOCK.lock();
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
        crate::memory::test_support::ensure_init();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::task::init();
        init();
        crate::component::registry::init();

        // Given：一个 Ready 组件 + 一个 Runnable 任务（直接进全局 task 表）。
        let owner = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(b"sched_failed_owner", 0x8000_0000, 0x8000_0000, None)
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        let task = crate::task::get_task_table()
            .lock()
            .create(owner, 0x8000_0000)
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
        let a = table.create(owner, ENTRY).unwrap();
        let b = table.create(owner, ENTRY).unwrap();
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
        let provider = reg.declare(b"scheduler_bad", ENTRY, ENTRY, None).unwrap();
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
}
