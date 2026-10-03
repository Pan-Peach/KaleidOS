//! Core 调度机制：本 CPU 候选快照 → SchedulerPolicy 提议 → 原子复验 / 提交 → 切换。
//! 职责契约见 `docs/architecture/scheduling.md`；RR / 公平性 / 私有队列属于组件。
//!
//! 每 CPU 拥有 current、锚点和 incoming IRQ 状态；全局 task table 是任务真相。
//! 决定阶段锁序为 registry → CPU state → task table；组件回调与 context switch
//! 期间不持 Core 锁。IRQ 关闭至 incoming 栈和身份安装完成，再恢复 incoming 的值。
//!
//! 同一 policy 的调用栈串行认领 / 归还；替换 busy policy 返回 EBUSY。过期候选重新
//! 取快照，非法提议 / 非零返回 / panic 才退役 provider。策略失败使用确定性回退；
//! 从未选择策略仍返回 NoPolicy。固定 CPU 防止寄存器保存完成前跨 CPU 重入。

use crate::component::abi::InterfaceAbi;
use crate::component::call;
use crate::component::containment::CallOutcome;
use crate::component::endpoint::{self, EndpointError, EndpointId, ExecutionDomain};
use crate::component::load::ComponentLoadError;
use crate::component::{ComponentId, containment, registry};
use crate::generated::abi::{
    KCOMP_SCHEDULER_NONE, KCOMP_SCHEDULER_POLICY_ABI, KCOMP_SCHEDULER_POLICY_CONTRACT,
    KCOMP_SCHEDULER_POLICY_NAME, KCOMP_SCHEDULER_TASK_ID_LEN, KcompCallFrame,
};
use crate::irq::IrqSaveGuard;
use crate::machine::CpuId;
use crate::memory::MemoryLease;
use crate::task::{self, TaskId, TaskState};
use alloc::boxed::Box;
use alloc::vec::Vec;
use arch::{ContextImpl, CpuArch, CpuImpl};
use spin::{Mutex, Once};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedError {
    /// 从未选择过 SchedulerPolicy（`kcore_sched_set_policy` 尚未成功）。已安装
    /// 策略失败 / 失效**不**是 `NoPolicy`：那时 Core 用确定性回退继续调度。
    NoPolicy,
    /// 任务表状态机拒绝推进（yield/exit 时当前任务不是 Running 等）。
    ///
    /// 也用于**上下文种类拒绝**：调度操作（含策略选择）不得在 IRQ 回调作用域、
    /// service-call 边界或 policy 执行内（含其下的嵌套边界）执行
    /// （`containment::scheduling_forbidden`，见 [`deny_scheduling_forbidden`]），
    /// ABI 上是 `-EINVAL`。复用本变体是为了不改内部错误枚举与唯一的 errno 映射表。
    InvalidTransition,
    /// 当前任务从表中消失（Core 不变式被破坏，不应发生）。
    NotFound,
    /// yield/exit 调用时本 CPU 没有在跑任务（只有任务能 yield/exit）。
    NoCurrent,
    /// `kcore_sched_set_policy` 的 endpoint 校验失败：未发布 / 已死 / owner 非
    /// `Ready` / contract·abi 不符。
    PolicyEndpoint(EndpointError),
    /// 策略 provider 的 image 没有 `kcomp_service_dispatch`：它不能作为策略
    /// provider（选择时与每次调用前都复验）。
    NoDispatcher,
    /// Core 无法为策略执行准备栈（`-ENOMEM`）；策略配置不变。
    NoPolicyStack,
    /// 策略 provider 不在 KernelNative 域：策略回调在 **Core 拥有的栈上、共享
    /// 内核 AS 里**执行（专用 PolicyCall 边界），Isolated / Sandbox provider 的
    /// dispatcher 是它们自己域内的 VA，Core 不能这样调用 → 显式拒绝（`-ENOTSUP`），
    /// 绝不静默按 KernelNative 语义执行（跨域 Gate 只覆盖通用
    /// service 调用，不覆盖调度 commit 路径）。
    PolicyUnsupportedDomain,
    /// The selected provider is currently executing; replacement must retry.
    PolicyBusy,
}

/// 上下文种类门禁：IRQ 回调（同步、不可 yield 的顶半部）与 service call
/// （provider 跑在 Core 拥有的 service stack 上，没有调度可见的任务）内，调度
/// 操作一律拒绝（返回 `-EINVAL`，绝不 panic），因为 `schedule_next` 会在 trap
/// 上下文 / 错误的任务上下文里做 context switch，且没有 Core 拥有的恢复点。
/// 门禁沿边界链**祖先遍历**（`Service → create → sched::run` 不能溜过）。
/// Core 机制层就拒绝，ABI 边界（`component/export.rs`）保持原样。
fn deny_scheduling_forbidden() -> Result<(), SchedError> {
    if containment::scheduling_forbidden() {
        return Err(SchedError::InvalidTransition);
    }
    Ok(())
}

/// 本 CPU 的调度真相：锚点上下文 + 当前任务。
///
/// `anchor` = 任务之外执行流（monitor / 组件 init 调用栈）的挂起上下文；
/// 全部任务退出后 CPU 回到这里。首次 `run()` 时捕获，之后每次耗尽任务
/// 都回到同一份（Box 地址稳定，跨切换有效）。
///
/// `CpuState` 是天然 per-CPU 单元——每个逻辑 CPU 一份（`CPU_TABLE`），彼此独立；
/// 任务表仍是全局共享真相，跨 CPU 互斥由 commit 路径负责（见 `docs/modules/arch.md`）。
struct CpuState {
    anchor: Option<Box<ContextImpl>>,
    current: Option<TaskId>,
    anchor_irq: Option<<CpuImpl as CpuArch>::IrqFlags>,
    incoming_irq: Option<<CpuImpl as CpuArch>::IrqFlags>,
}

/// 每逻辑 CPU 一份调度真相，索引 = 逻辑 `CpuId`。**UP = 只有第 0 项的 SMP**，
/// 不再有单独的单一 `CPU`（见 `docs/modules/arch.md` 的 UP = SMP-1）。
static CPU_TABLE: Once<crate::smp::PerCpu<Mutex<CpuState>>> = Once::new();

/// 调度策略配置（Core truth）：**只记 EndpointId** + 选择时为策略执行准备的
/// Core-owned 栈。
///
/// - `endpoint == None` = 从未配置：`pick_next` 返回 `NoPolicy`（绝不退化到内置
///   调度器）；
/// - `retired == true` = 已安装策略失败 / 失效：确定性回退（id 序首项）生效，
///   直到显式重新选择——不会在下次调度时退化成 `NoPolicy`；
/// - `stack` 只在 Armed 状态下非空。panic 的栈被保留（`mem::forget`，退役、
///   绝不复用）；正常返回的栈留在槽里复用；退役时立即释放。调用中 stack 被认领且 busy=true；重新选择 = 准备新栈。
struct PolicySlot {
    endpoint: Option<EndpointId>,
    stack: Option<MemoryLease>,
    retired: bool,
    busy: bool,
}

static POLICY: Once<Mutex<PolicySlot>> = Once::new();

pub fn init() {
    CPU_TABLE.call_once(|| {
        crate::smp::PerCpu::new(crate::machine::MAX_CPUS, |_| {
            Mutex::new(CpuState {
                anchor: None,
                current: None,
                anchor_irq: None,
                incoming_irq: None,
            })
        })
        .expect("sched per-cpu table allocation failed")
    });
    POLICY.call_once(|| {
        Mutex::new(PolicySlot {
            endpoint: None,
            stack: None,
            retired: false,
            busy: false,
        })
    });
}

/// 当前执行 CPU 的逻辑身份（由 arch 入口记录解析；UP 恒为 CPU0）。
fn current_cpu_id() -> CpuId {
    crate::smp::current_cpu()
}

/// 取**当前执行 CPU** 的调度真相。锁纪律不变（调用点仍只短暂持锁）。
fn cpu() -> &'static Mutex<CpuState> {
    CPU_TABLE
        .get()
        .expect("sched not initialized")
        .get(current_cpu_id())
        .expect("current cpu outside the per-cpu scheduler table")
}

/// 按逻辑 CPU 取调度真相槽（`PerCpu` 已为全部 `MAX_CPUS` 构造，故恒存在）。
fn cpu_slot(cpu: CpuId) -> Option<&'static Mutex<CpuState>> {
    CPU_TABLE.get().and_then(|table| table.get(cpu))
}

/// 初始化某个 CPU 的调度状态（AP 在本地启动时调用；UP 不调用）。
///
/// 每 CPU `CpuState` 已在 [`init`] 里为全部槽位构造；本函数把**本 CPU** 的
/// `anchor` / `current` 复位为空（AP 首次进入调度前的干净起点）。必须由目标
/// CPU 自己调用——`CpuState` 是 CPU-local 执行状态，不能跨 CPU 初始化。
#[allow(dead_code)]
pub(crate) fn init_cpu(cpu: CpuId) -> Result<(), SchedError> {
    if current_cpu_id() != cpu {
        return Err(SchedError::InvalidTransition);
    }
    let slot = cpu_slot(cpu).ok_or(SchedError::NotFound)?;
    let mut state = slot.lock();
    state.anchor = None;
    state.current = None;
    state.anchor_irq = None;
    state.incoming_irq = None;
    Ok(())
}

/// 请求目标 CPU 在**安全边界**重新调度（不在 IPI 回调里切上下文；UP 不调用）。
#[allow(dead_code)]
pub(crate) fn request_reschedule(target: CpuId) -> Result<(), SchedError> {
    let Some(record) = crate::smp::record(target) else {
        // Host/early UP has no published SMP topology yet.
        return if target == current_cpu_id() {
            Ok(())
        } else {
            Err(SchedError::NotFound)
        };
    };
    record.set_resched();
    if target != current_cpu_id() {
        let mut targets = crate::smp::CpuMask::empty();
        targets.insert(target).map_err(|_| SchedError::NotFound)?;
        crate::smp::ipi::notify(&targets, crate::smp::ipi::IpiRequest::Reschedule)
            .map_err(|_| SchedError::NotFound)?;
    }
    Ok(())
}

fn policy() -> &'static Mutex<PolicySlot> {
    POLICY.get().expect("sched not initialized")
}

/// 当前 CPU 正在运行的任务。没有进入任务执行流时返回 `None`（锚点上下文）。
///
/// 这是 Core 读取执行身份的入口；组件不能通过它修改调度状态。
pub fn current_task() -> Option<TaskId> {
    let _irq = IrqSaveGuard::new();
    cpu().lock().current
}

/// 收集**本 CPU 可认领**的 Runnable 且 **owner 仍是活实例**的任务（BTreeMap 迭代序
/// = id 升序；列表内容由 Core 决定，调度器只读这份裁剪过的输入）。
///
/// 任务在 start 时固定到目标 CPU，Core 提供的候选不会交给其它 CPU。
/// Core 内部尚未设置归属的 fresh context 也必须在首次 dispatch 时固定归属。
///
/// 组件失败 = 逻辑死亡：`Failed` 组件的任务必须从候选中剔除，否则调度器会把 CPU
/// 交给一个已经死掉的实例。两把锁**先后分开**取（先 task 表快照 owner、再 registry
/// 判定），不做嵌套，避免与 create_task（registry → task_table）的锁序冲突。
fn collect_runnable() -> Vec<TaskId> {
    collect_claimable_for(current_cpu_id())
}

/// 收集逻辑 CPU `cpu` 可认领的 Runnable 任务（owner 存活）。
fn collect_claimable_for(cpu: CpuId) -> Vec<TaskId> {
    let _irq = IrqSaveGuard::new();
    let candidates: Vec<(TaskId, ComponentId)> = {
        let table = task::get_task_table().lock();
        table
            .iter()
            .filter(|(_, r)| r.state() == TaskState::Runnable && r.claimable_by(cpu))
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

/// 逻辑 CPU `cpu` 是否有可认领的 Runnable 任务——AP 空闲循环「要不要进调度」的门。
///
/// 按 registry → task 顺序验证 owner，失败任务不能使 idle 永久自旋。
pub(crate) fn has_claimable_for(cpu: CpuId) -> bool {
    let _irq = IrqSaveGuard::new();
    let reg = registry::get_registry().lock();
    task::get_task_table().lock().iter().any(|(_, r)| {
        r.state() == TaskState::Runnable && r.claimable_by(cpu) && reg.may_run(r.owner())
    })
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

/// 选择调度策略（`kcore_sched_set_policy` 的 Core 实现）：把 `endpoint` 提交为
/// 调度配置，**只记 EndpointId**——它不是 publish，也不做全局名字发现。
///
/// 校验（`registry → endpoints → images` 锁内，全部释放后才准备执行栈）：
/// endpoint 存活 + contract + abi exact-match + provider 有
/// `kcomp_service_dispatch`（策略 provider 只提供 image 级入口；Direct 的
/// `api` / `ctx` 不参与策略执行）。
///
/// 上下文门禁：IRQ / service-call / policy 执行内拒绝（`-EINVAL`）——普通组合 /
/// create 上下文可以选择一个已经 `Ready` 的 provider。选择必须在 provider 的
/// create 返回 0 **之后**（endpoint 只在 staged publish 原子提交后存在）。
pub fn set_policy(endpoint: EndpointId) -> Result<(), SchedError> {
    // (1) 上下文门禁：调度 commit 路径内部 / IRQ / service call 不得改配置。
    deny_scheduling_forbidden()?;

    // (2) 锁内校验：contract + abi + 存活 + provider 必须有 dispatcher。
    {
        let components = registry::get_registry().lock();
        let endpoints = endpoint::get_endpoints().lock();
        let record = endpoints
            .lookup(
                &components,
                endpoint,
                crate::component::endpoint::ContractId::from_raw(KCOMP_SCHEDULER_POLICY_CONTRACT),
                InterfaceAbi::from_raw(KCOMP_SCHEDULER_POLICY_ABI),
            )
            .map_err(SchedError::PolicyEndpoint)?;
        let instance = components
            .get(record.owner)
            .ok_or(SchedError::PolicyEndpoint(EndpointError::ProviderNotFound))?;
        // 部署域门禁：策略执行边界是 Core 栈 + 共享内核 AS，Isolated / Sandbox
        // provider 的 dispatcher VA 在那里没有意义 → 显式拒绝（绝不静默降级）。
        if instance.execution_domain != ExecutionDomain::KernelNative {
            return Err(SchedError::PolicyUnsupportedDomain);
        }
        if instance.loaded.service_dispatch.is_none() {
            return Err(SchedError::NoDispatcher);
        }
    }

    // (3) 策略执行栈在**策略执行之外**准备（锁已全部释放）。
    let stack = containment::alloc_policy_stack().ok_or(SchedError::NoPolicyStack)?;

    // (4) 提交配置：只记 EndpointId + 准备好的栈。旧栈（若有）随替换释放——
    //     策略调用是同步的，且 policy 执行内拒绝替换，故旧栈不在使用中。
    let mut slot = policy().lock();
    if slot.busy {
        return Err(SchedError::PolicyBusy);
    }
    slot.endpoint = Some(endpoint);
    slot.stack = Some(stack);
    slot.retired = false;
    drop(slot);
    // Tasks may have been published before a policy was selected.
    for cpu in crate::smp::online_cpus().iter() {
        let _ = request_reschedule(cpu);
    }
    Ok(())
}

/// 组合辅助（monitor / ArchTest）：在 `provider` 实例上发现 `scheduler.policy`
/// endpoint 并选择它。
///
/// 组合方**显式**做这一步（look up + select）；Core 的调度路径绝不按名字发现。
pub fn select_provider(provider: ComponentId) -> Result<(), SchedError> {
    let endpoint = {
        let components = registry::get_registry().lock();
        let endpoints = endpoint::get_endpoints().lock();
        endpoints
            .discover(
                &components,
                provider,
                KCOMP_SCHEDULER_POLICY_NAME,
                crate::component::endpoint::ContractId::from_raw(KCOMP_SCHEDULER_POLICY_CONTRACT),
            )
            .map_err(SchedError::PolicyEndpoint)?
    };
    set_policy(endpoint)
}

/// 确定性回退：id 序首项，且**提交前验证**它的 owner 仍允许运行。
///
/// 回退任务恰好属于刚失败的策略 provider 时返回 `None`（回锚点）——绝不把 CPU
/// 交给已死实例的任务，也绝不把"已安装策略失败"退化成 `NoPolicy`。
fn fallback_task(runnable: &[TaskId]) -> Option<TaskId> {
    runnable
        .first()
        .copied()
        .filter(|id| owner_still_runnable(*id))
}

/// 策略配置退役（失败 / 失效）：确定性回退生效，直到显式重新选择。
///
/// 栈的处理：`Some`（正常返回 / 准备失败，未被 panic 污染）立即释放——策略已
/// 退役、不会再被调用；`None` 是 panic 路径保留的栈（边界内已 `mem::forget`），
/// 既不能释放也绝不复用。
fn retire_policy(stack: Option<MemoryLease>) {
    drop(stack);
    let mut slot = policy().lock();
    slot.retired = true;
    slot.busy = false;
    slot.stack = None;
}

/// 策略调用正常返回：把准备好的栈放回槽里复用（配置保持 Armed）。
fn restore_policy_stack(stack: Option<MemoryLease>) {
    let mut slot = policy().lock();
    slot.stack = stack;
    slot.busy = false;
}

struct Choice {
    task: Option<TaskId>,
    provider: Option<ComponentId>,
}

impl Choice {
    fn fallback(task: Option<TaskId>) -> Self {
        Self {
            task,
            provider: None,
        }
    }
}

#[cfg(test)]
fn pick_next(runnable: &[TaskId]) -> Result<Option<TaskId>, SchedError> {
    choose_next(runnable).map(|choice| choice.task)
}

/// 请求策略提议下一个任务。返回 `None` = 没有可运行任务（回锚点）。
///
/// - **从未配置** → `NoPolicy`（绝不猜、绝不内置调度器）；
/// - **已安装策略失效 / 失败** → 确定性回退（[`fallback_task`]），不再调用组件；
/// - **提议非法**（提议 id 不在 Core 裁剪过的 runnable 列表内）或 provider 返回
///   非 0 → provider **endpoint-aware 失败**（逻辑死亡 + authority 回收 + 全部
///   endpoint 永久失效）+ 配置退役 + 确定性回退——一个完全错误的调度器组件不能
///   挂起调度，也不能把 CPU 交给不存在的任务；
/// - **provider panic** → panic 收尾已在 `call::call_policy` 内完成（含 inflight
///   归还）；这里只退役配置 + 回退。panic 逃逸回**本调度帧**（它仍持有
///   `IrqSaveGuard`），绝不穿越它，也不继承让出 CPU 的任务的边界。
fn choose_next(runnable: &[TaskId]) -> Result<Choice, SchedError> {
    if runnable.is_empty() {
        return Ok(Choice::fallback(None));
    }

    // (1) 配置快照：endpoint + 选择时准备好的栈（锁只在这一小段持有）。
    let (endpoint, stack) = loop {
        let mut slot = policy().lock();
        if slot.busy {
            drop(slot);
            core::hint::spin_loop();
            continue;
        }
        let Some(endpoint) = slot.endpoint else {
            return Err(SchedError::NoPolicy);
        };
        if slot.retired {
            return Ok(Choice::fallback(fallback_task(runnable)));
        }
        match slot.stack.take() {
            Some(stack) => {
                slot.busy = true;
                break (endpoint, stack);
            }
            None => {
                // 配置损坏（Armed 却没有栈）：退役 + 回退，绝不 panic。
                slot.retired = true;
                return Ok(Choice::fallback(fallback_task(runnable)));
            }
        }
    };

    // (2) 锁内准备（registry → endpoints → images），全部释放后才调用组件。
    let target = match call::prepare_policy(endpoint) {
        Ok(target) => target,
        Err(_) => {
            // 已安装策略的 endpoint 失效 / provider 离开 Ready：退役 + 回退。
            retire_policy(Some(stack));
            return Ok(Choice::fallback(fallback_task(runnable)));
        }
    };

    // (3) 构造 wire frame（Core 编码；分配在这里，不在回调内——见
    //     `abi/scheduler.toml` 的 CHOOSE_NEXT 格式）。
    let current = cpu()
        .lock()
        .current
        .map_or(KCOMP_SCHEDULER_NONE, |id| id.raw());
    let mut args = [0u8; crate::generated::abi::KCOMP_SCHEDULER_ARGS_LEN];
    args[..4].copy_from_slice(&current.to_le_bytes());
    args[4..].copy_from_slice(&(current_cpu_id().raw() as u32).to_le_bytes());
    let mut input = Vec::with_capacity(runnable.len() * KCOMP_SCHEDULER_TASK_ID_LEN);
    for id in runnable {
        input.extend_from_slice(&id.raw().to_le_bytes());
    }
    let mut output = [0u8; KCOMP_SCHEDULER_TASK_ID_LEN];
    let frame = KcompCallFrame {
        args: args.as_ptr(),
        args_len: args.len(),
        input: input.as_ptr(),
        input_len: input.len(),
        output: output.as_mut_ptr(),
        output_len: output.len(),
    };

    // (4) 无锁调用：专用 PolicyCall 边界（Core 是 caller；panic 逃逸回本帧）。
    let call::PolicyCallOutcome { outcome, stack } = call::call_policy(&target, &frame, stack);

    match outcome {
        CallOutcome::Returned(0) => {
            let proposed = TaskId::from_raw(u32::from_le_bytes(output));
            crate::trace::emit(crate::trace::TraceEvent::PolicyProposal {
                component: target.owner,
                task: proposed,
            });
            if runnable.contains(&proposed) {
                restore_policy_stack(stack);
                return Ok(Choice {
                    task: Some(proposed),
                    provider: Some(target.owner),
                });
            }
            // 组件提出非法提议：endpoint-aware 隔离 + 退役 + 回退。
            crate::trace::emit(crate::trace::TraceEvent::PolicyRejected {
                component: target.owner,
                reason: crate::trace::RejectReason::NotRunnable,
            });
            crate::component::fail_component(target.owner, ComponentLoadError::PolicyRejected);
            retire_policy(stack);
            Ok(Choice::fallback(fallback_task(runnable)))
        }
        // provider 返回非 0（契约违约 / 内部错误）：提议不可用 → 同一档失败。
        // 不发 trace 事件：`RejectReason` 的词表只描述"提议被 Core 拒绝"，
        // 没有描述"provider 自己报告失败"的语义（新增 reason = ABI 变更）；
        // 失败本身仍可从 `ComponentState{Failed}` 事件观察到。
        CallOutcome::Returned(_) => {
            crate::component::fail_component(target.owner, ComponentLoadError::PolicyRejected);
            retire_policy(stack);
            Ok(Choice::fallback(fallback_task(runnable)))
        }
        // panic 收尾（Failed + endpoint 失效 + inflight 归还）已在 `call_policy`
        // 内完成；`stack == None`（保留、退役）。这里只退役配置 + 回退。
        CallOutcome::Panicked => {
            retire_policy(stack);
            Ok(Choice::fallback(fallback_task(runnable)))
        }
        // 不可能：栈是选择时准备好的。防御性退役 + 回退，绝不 panic。
        CallOutcome::NoStack => {
            retire_policy(stack);
            Ok(Choice::fallback(fallback_task(runnable)))
        }
    }
}

/// 核心切换：from（当前任务或锚点）→ to（策略提议或锚点）。
///
/// `from = Some(id)`：把当前任务推进到 `after`（yield→Runnable / exit→Exited）
/// 并保存它的上下文；`from = None`：捕获锚点上下文（首次 run）。
/// `next = None`：没有可运行任务，切回锚点。
///
/// `abort` 非空时，先撤销失败 owner 的 admission，再收集候选；同 owner 的
/// 其它任务不会作为后继提交。失败的 scheduler provider 也经退役 / 回退维持进展。
///
/// 锁纪律：`context_switch` 前全部锁释放；锁外先 revoke、再按**目标上下文**
/// 安装逃逸 guard，最后切换。
fn schedule_next(
    from: Option<TaskId>,
    after: Option<TaskState>,
    abort: Option<(TaskId, ComponentId)>,
) -> Result<(), SchedError> {
    let guard = IrqSaveGuard::new();
    schedule_next_with_guard(from, after, abort, guard)
}

fn schedule_next_with_guard(
    from: Option<TaskId>,
    after: Option<TaskState>,
    abort: Option<(TaskId, ComponentId)>,
    guard: IrqSaveGuard,
) -> Result<(), SchedError> {
    // Revoke admission before taking any successor snapshot. This also
    // excludes siblings on other CPUs at their next scheduling boundary.
    if let Some((dead, owner)) = abort {
        crate::component::fail_component(owner, ComponentLoadError::TaskPanicked(dead));
    }
    #[cfg(test)]
    assert!(
        task::get_task_table().try_lock().is_some(),
        "schedule_next must not be entered while holding the TaskTable lock"
    );
    // Policy sees a snapshot. Registry -> CPU slot -> task table protects
    // admission and the two-sided commit; no lock spans provider execution.
    let (from_ptr, to_ptr, next, next_owner) = loop {
        let runnable = collect_runnable();
        let choice = choose_next(&runnable)?;
        let mut next = choice.task;
        let reg = registry::get_registry().lock();
        let mut cpu_guard = cpu().lock();
        let mut table = task::get_task_table().lock();
        if cpu_guard.current != from {
            return Err(SchedError::InvalidTransition);
        }
        if let Some(id) = next
            && table.get(id).is_none_or(|r| !reg.may_run(r.owner()))
        {
            next = None;
        }
        let mut after = after.clone();
        if let Some(id) = from
            && table.get(id).is_some_and(|r| !reg.may_run(r.owner()))
        {
            after = Some(TaskState::Exited);
        }
        match table
            .commit_switch(current_cpu_id(), from, after, next)
            .map_err(|_| SchedError::InvalidTransition)?
        {
            task::table::SwitchCommit::StaleProposal => continue,
            task::table::SwitchCommit::ParkPermit => return Ok(()),
            task::table::SwitchCommit::Committed => {}
        }
        if let (Some(component), Some(task)) = (choice.provider, next) {
            // Acceptance records committed truth, never a stale snapshot.
            crate::trace::emit(crate::trace::TraceEvent::PolicyAccepted { component, task });
        }
        if from.is_none() && next.is_none() {
            return Ok(());
        }
        let outgoing_irq = guard.into_flags();
        let from_ptr = if let Some(id) = from {
            let record = table.get_mut(id).expect("validated outgoing task");
            record.irq_flags = Some(outgoing_irq);
            record.context.as_mut() as *mut ContextImpl
        } else {
            cpu_guard.anchor_irq = Some(outgoing_irq);
            if cpu_guard.anchor.is_none() {
                cpu_guard.anchor = Some(Box::new(CpuImpl::new_context(0, 0)));
            }
            cpu_guard.anchor.as_mut().unwrap().as_mut() as *mut ContextImpl
        };
        let (to_ptr, owner) = if let Some(id) = next {
            let record = table.get_mut(id).expect("validated incoming task");
            cpu_guard.incoming_irq = record.irq_flags.take();
            (
                record.context.as_mut() as *mut ContextImpl,
                Some(record.owner()),
            )
        } else {
            cpu_guard.incoming_irq = cpu_guard.anchor_irq.take();
            (
                cpu_guard.anchor.as_mut().expect("saved anchor").as_mut() as *mut ContextImpl,
                None,
            )
        };
        cpu_guard.current = next;
        break (from_ptr, to_ptr, next, owner);
    };

    // IRQs stay masked until the incoming stack and escape guard are ready.
    if let Some(id) = next {
        crate::trace::emit(crate::trace::TraceEvent::TaskSwitch { from, to: id });
    }
    let suspended_depth = containment::core_abi_depth();
    match next {
        Some(id) => containment::enter_task(id, next_owner.expect("task owner")),
        None => containment::enter_anchor(),
    }
    // SAFETY: stable resident records; all locks are released. Task placement
    // prevents another CPU entering a context before its outgoing save finishes.
    unsafe { CpuImpl::context_switch(&mut *from_ptr, &*to_ptr) };
    containment::resume_core_abi_depth(suspended_depth);
    finish_switch();
    Ok(())
}

/// Runs on the incoming stack, with metadata installed and IRQs still masked.
/// Fresh tasks call it from task_entry_trampoline; suspended executions call it
/// immediately after context_switch. IRQ flags belong to the incoming execution.
pub(crate) fn finish_switch() {
    let flags = cpu().lock().incoming_irq.take();
    match flags {
        Some(flags) => CpuImpl::restore_irq(flags),
        None => CpuImpl::enable_irq(),
    }
    // A remote failure can race a committed switch. At this cooperative
    // boundary stop the dead owner before returning to component code.
    if let Some(id) = current_task()
        && !owner_still_runnable(id)
    {
        let _ = exit_current();
    }
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
    deny_scheduling_forbidden()?;
    if current_task().is_some() {
        return Err(SchedError::InvalidTransition);
    }
    if collect_runnable().is_empty() {
        return Ok(());
    }
    schedule_next(None, None, None)
}

/// Service BSP anchor work at monitor/console safe points.
/// Returns true when runnable component work was serviced successfully.
pub(crate) fn service_local() -> bool {
    if CPU_TABLE.get().is_some()
        && !containment::scheduling_forbidden()
        && current_task().is_none()
        && has_claimable_for(current_cpu_id())
    {
        run().is_ok()
    } else {
        false
    }
}

/// 当前任务主动让出 CPU：Running → Runnable，切换走。再次被选中时返回。
pub fn yield_current() -> Result<(), SchedError> {
    deny_scheduling_forbidden()?;
    let current = cpu().lock().current.ok_or(SchedError::NoCurrent)?;
    schedule_next(Some(current), Some(TaskState::Runnable), None)
}

/// 当前任务退出：Running → Exited，切换走。**本任务从此不再恢复**——
/// 若还有 Runnable 任务则它们接管；全部退出后控制权回到锚点。
pub fn exit_current() -> Result<(), SchedError> {
    deny_scheduling_forbidden()?;
    let current = cpu().lock().current.ok_or(SchedError::NoCurrent)?;
    schedule_next(Some(current), Some(TaskState::Exited), None)
}

/// 阻塞当前任务，直到其它执行流调用 [`unpark_task`]。
///
/// 只有任务上下文可以 park；成功切走后，本调用会在任务被重新调度时返回。
/// 快速路径先消费 permit；最终检查与 `Running → Blocked` 在同一次表锁事务内，
/// 覆盖策略执行期间的远端 unpark。IRQ guard 转交保存值给 outgoing execution，
/// 切换不携带 guard；在 incoming 栈上恢复该执行流自己的 IRQ 状态。
pub fn park_current() -> Result<(), SchedError> {
    deny_scheduling_forbidden()?;
    let guard = IrqSaveGuard::new();
    let current = cpu().lock().current.ok_or(SchedError::NoCurrent)?;
    let has_permit = {
        let mut table = task::get_task_table().lock();
        table
            .consume_park_pending(current)
            .map_err(|_| SchedError::NotFound)?
    };
    if has_permit {
        return Ok(());
    }
    schedule_next_with_guard(Some(current), Some(TaskState::Blocked), None, guard)
}

/// 唤醒同一 owner 的任务，或为尚未 park 的任务记一份 pending permit。
///
/// 不在此处切换 CPU，因此可从 IRQ 回调调用；策略回调不得修改任务状态。
/// 实现时不仅本路径要在取任务表锁前 irq-save，所有可能被此 IRQ 打断并持有
/// TaskTable 锁的路径也必须避免同核重入死锁。
pub fn unpark_task(
    requester: ComponentId,
    task_id: TaskId,
) -> Result<(), crate::task::error::TaskError> {
    if containment::policy_call_in_chain() {
        return Err(crate::task::error::TaskError::InvalidTransition);
    }
    let (outcome, target) = {
        let _irq = IrqSaveGuard::new();
        let registry = registry::get_registry().lock();
        let mut table = task::get_task_table().lock();
        let record = table.get(task_id).ok_or(task::TaskError::NotFound)?;
        if record.owner() != requester {
            return Err(task::TaskError::WrongOwner);
        }
        if !registry.may_run(requester) {
            return Err(task::TaskError::RequesterNotReady);
        }
        let outcome = table.unpark(requester, task_id)?;
        let target = table.get(task_id).and_then(|r| r.home_cpu());
        (outcome, target)
    };
    if outcome == task::table::UnparkOutcome::Woke
        && let Some(target) = target
        && let Err(error) = request_reschedule(target)
    {
        crate::log!(
            "sched",
            "wake committed; CPU {} notification failed: {:?}",
            target.raw(),
            error
        );
    }
    Ok(())
}

/// 时钟抢占入口（`timer::on_trap` 调用；中断上下文）。
///
/// # 抢占模型（未选型，当前 `todo!()`）
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
    use crate::component::ComponentState;
    use crate::component::abi::InterfaceKind;
    use crate::component::call::CallError;
    use crate::component::containment;
    use crate::component::endpoint::{ContractId, EndpointError, ExecutionDomain};
    use crate::component::registry;
    use crate::errno::Errno;
    use crate::generated::abi::KCOMP_SCHEDULER_METHOD_CHOOSE_NEXT;
    use crate::task::TaskError;
    use crate::test_support::{Rank, TestLock};
    use core::ptr;
    use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    /// 串行化触碰进程全局 task table / registry / 调度策略配置的调度测试。
    ///
    /// rank = SCHED（模块本地、最外层；见 [`crate::test_support`]）。
    static SCHED_TEST_LOCK: TestLock = TestLock::new(Rank::Sched);

    const ENTRY: usize = 0x8000_0000;

    // ------------------------------------------------------------------
    // 全局真相初始化 / 复位
    // ------------------------------------------------------------------

    /// 初始化本模块测试需要的进程级真相（幂等），清空策略配置并回到锚点边界。
    ///
    /// 策略配置是进程级 `Once`：一次选择会残留到后续用例，因此每个用例显式
    /// 清空（调度用例由 [`SCHED_TEST_LOCK`] 串行化）。
    fn init_world() {
        crate::memory::test_support::ensure_init();
        crate::task::init();
        init();
        registry::init();
        endpoint::init();
        crate::resource::init();
        clear_policy();
        reset_cpu();
        containment::enter_anchor();
    }

    #[test]
    fn concurrent_policy_calls_share_one_stack_without_retiring_provider() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();
        struct Pause {
            entered: std::sync::Barrier,
            release: std::sync::Barrier,
        }
        extern "C" fn paused(
            state: *mut (),
            port: u32,
            method: u32,
            frame: *const KcompCallFrame,
        ) -> i32 {
            let pause = unsafe { &*state.cast::<Pause>() };
            pause.entered.wait();
            pause.release.wait();
            first_runnable(ptr::null_mut(), port, method, frame)
        }
        let pause = Pause {
            entered: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        };
        let (provider, endpoint) = install_policy(
            b"sched_concurrent",
            paused as *const () as usize,
            (&pause as *const Pause).cast_mut().cast(),
        );
        let owner = ready_component(b"sched_concurrent_owner");
        let mut cleanup = TaskCleanup::new();
        let a = runnable_task(owner);
        cleanup.track(a);
        let b = crate::task::get_task_table()
            .lock()
            .create(owner, ENTRY, ptr::null_mut())
            .unwrap();
        cleanup.track(b);
        crate::task::get_task_table()
            .lock()
            .start_on(owner, b, CpuId(1))
            .unwrap();
        std::thread::scope(|scope| {
            let child = scope.spawn(|| {
                unsafe { CpuImpl::install_per_cpu_base(CpuId(1), core::ptr::NonNull::dangling()) };
                pause.entered.wait();
                let replacement = set_policy(endpoint);
                pause.release.wait();
                let next =
                    containment::with_test_policy_dispatch(first_runnable, || pick_next(&[b]));
                assert_eq!(replacement, Err(SchedError::PolicyBusy));
                assert_eq!(next, Ok(Some(b)));
            });
            assert_eq!(
                containment::with_test_policy_dispatch(paused, || pick_next(&[a])),
                Ok(Some(a))
            );
            child.join().unwrap();
        });
        assert!(!policy().lock().retired);
        assert!(!policy().lock().busy);
        assert_eq!(
            registry::get_registry().lock().get(provider).unwrap().state,
            ComponentState::Ready
        );
        clear_policy();
    }

    /// 清空调度策略配置（丢弃准备好的执行栈）。
    fn clear_policy() {
        let mut slot = policy().lock();
        slot.endpoint = None;
        slot.stack = None;
        slot.retired = false;
        slot.busy = false;
    }

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

    // ------------------------------------------------------------------
    // 实例 / endpoint / 任务
    // ------------------------------------------------------------------

    /// 全局 registry 里的一个 `Ready` 活组件（id 跨用例累积）。
    fn ready_component(name: &[u8]) -> ComponentId {
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(
                name,
                crate::component::registry::test_support::test_loaded(0, None),
                ExecutionDomain::KernelNative,
            )
            .unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    /// 一个 Ready 的策略 provider：loaded image 带（可选）dispatcher，registry
    /// 记录 opaque instance state。
    fn ready_policy_provider(
        name: &[u8],
        dispatcher: Option<usize>,
        state: *mut (),
    ) -> ComponentId {
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(
                name,
                crate::component::registry::test_support::test_loaded(0, dispatcher),
                ExecutionDomain::KernelNative,
            )
            .unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        reg.record_instance_state(id, state).unwrap();
        id
    }

    /// 发布并提交一个名为 `scheduler.policy` 的 endpoint（contract / abi 由调用方
    /// 给定，discover 也按同一 contract）。
    fn publish_policy_endpoint_at(
        provider: ComponentId,
        port: u32,
        contract: ContractId,
        abi: InterfaceAbi,
    ) -> EndpointId {
        let reg = registry::get_registry().lock();
        let mut eps = endpoint::get_endpoints().lock();
        eps.stage_publish(
            &reg,
            provider,
            KCOMP_SCHEDULER_POLICY_NAME,
            contract,
            InterfaceKind::Policy,
            abi,
            port,
            ptr::null(),
            ptr::null_mut(),
        )
        .unwrap();
        eps.commit_pending(&reg, provider).unwrap();
        eps.discover(&reg, provider, KCOMP_SCHEDULER_POLICY_NAME, contract)
            .unwrap()
    }

    /// 发布并提交 `scheduler.policy` endpoint（contract + abi 精确匹配）。
    fn publish_policy_endpoint(provider: ComponentId, port: u32) -> EndpointId {
        publish_policy_endpoint_at(
            provider,
            port,
            ContractId::from_raw(KCOMP_SCHEDULER_POLICY_CONTRACT),
            InterfaceAbi::from_raw(KCOMP_SCHEDULER_POLICY_ABI),
        )
    }

    /// 完整组合：Ready provider + endpoint + `set_policy`（成功即 Armed）。
    fn install_policy(name: &[u8], dispatcher: usize, state: *mut ()) -> (ComponentId, EndpointId) {
        let provider = ready_policy_provider(name, Some(dispatcher), state);
        let endpoint = publish_policy_endpoint(provider, 0);
        set_policy(endpoint).expect("policy selection must succeed");
        (provider, endpoint)
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

    struct TaskCleanup(alloc::vec::Vec<TaskId>);

    impl TaskCleanup {
        fn new() -> Self {
            Self(alloc::vec::Vec::new())
        }

        fn track(&mut self, task: TaskId) {
            self.0.push(task);
        }
    }

    impl Drop for TaskCleanup {
        fn drop(&mut self) {
            containment::enter_anchor();
            reset_cpu();
            clear_policy();
            let mut table = crate::task::get_task_table().lock();
            for task in self.0.drain(..) {
                let _ = table.remove(task);
            }
        }
    }

    /// 真实调度候选必须排除 Blocked；unpark 提交为 Runnable 后才重新可选。
    #[test]
    fn blocked_task_is_excluded_until_runnable_again() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        let owner = ready_component(b"sched_blocked_candidate");
        let a = runnable_task(owner);
        let b = runnable_task(owner);
        crate::task::get_task_table()
            .lock()
            .transition(a, TaskState::Running(CpuId(0)))
            .unwrap();
        crate::task::get_task_table()
            .lock()
            .transition(a, TaskState::Blocked)
            .unwrap();
        assert_eq!(collect_runnable(), alloc::vec![b]);

        crate::task::get_task_table()
            .lock()
            .transition(a, TaskState::Runnable)
            .unwrap();
        assert_eq!(collect_runnable(), alloc::vec![a, b]);

        remove_task(a);
        remove_task(b);
    }

    /// `init_cpu` 只复位**当前** CPU 的调度槽；跨 CPU 调用被拒，且不动远端槽位。
    #[test]
    fn init_cpu_resets_only_the_current_cpu_slot() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        // 污染当前 CPU（host = CPU0）的 `current`：init_cpu 必须清回空锚点。
        set_current(Some(TaskId::from_raw(1)));
        assert_eq!(init_cpu(CpuId::from_raw(0)), Ok(()));
        assert_eq!(current_task(), None);

        // 跨 CPU 调用被拒（host 当前 CPU 恒为 0）。
        assert_eq!(
            init_cpu(CpuId::from_raw(1)),
            Err(SchedError::InvalidTransition)
        );
    }

    /// 可认领性过滤：未运行过的任务任何 CPU 可认领；跑过一次的只认其上次 CPU。
    #[test]
    fn claimable_filter_pins_ran_tasks_to_their_cpu() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        let owner = ready_component(b"sched_claim_owner");
        let task = runnable_task(owner);

        // 从未运行过：任何 CPU 都能认领。
        assert!(has_claimable_for(CpuId::from_raw(0)));
        assert!(has_claimable_for(CpuId::from_raw(1)));

        // 在 CPU1 上跑过再 yield：只认 CPU1。
        crate::task::get_task_table()
            .lock()
            .transition(task, TaskState::Running(CpuId::from_raw(1)))
            .unwrap();
        crate::task::get_task_table()
            .lock()
            .transition(task, TaskState::Runnable)
            .unwrap();
        assert!(!has_claimable_for(CpuId::from_raw(0)), "ran on CPU1");
        assert!(has_claimable_for(CpuId::from_raw(1)));

        remove_task(task);
    }

    /// 端到端调度提交：A park 后 B 接手；unpark(A) 后 B yield，A 成为下一个 Running。
    /// Fake 后端不执行任务入口，但会检查切换时 IRQ 已恢复、任务表锁已释放。
    #[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
    #[test]
    fn park_then_unpark_switches_to_other_task_and_back() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();
        let mut cleanup = TaskCleanup::new();

        install_policy(
            b"sched_park_unpark_policy",
            first_runnable as *const () as usize,
            ptr::null_mut(),
        );
        let owner = ready_component(b"sched_park_unpark_owner");
        let a = runnable_task(owner);
        cleanup.track(a);
        let b = runnable_task(owner);
        cleanup.track(b);
        crate::task::get_task_table()
            .lock()
            .transition(a, TaskState::Running(CpuId(0)))
            .unwrap();
        set_current(Some(a));
        containment::enter_task(a, owner);

        assert_eq!(park_current(), Ok(()));
        assert_eq!(
            arch::fake::take_last_switch_irq_enabled_for_test(),
            Some(false),
            "IRQ stays masked until incoming metadata and stack are installed"
        );
        assert!(arch::fake::irq_enabled_for_test());
        assert_eq!(state_of(a), TaskState::Blocked);
        assert_eq!(state_of(b), TaskState::Running(CpuId(0)));
        assert_eq!(current_task(), Some(b));

        assert_eq!(unpark_task(owner, a), Ok(()));
        assert_eq!(state_of(a), TaskState::Runnable);
        assert_eq!(yield_current(), Ok(()));
        assert_eq!(
            arch::fake::take_last_switch_irq_enabled_for_test(),
            Some(false),
            "IRQ stays masked through the context switch"
        );
        assert!(arch::fake::irq_enabled_for_test());
        assert_eq!(state_of(a), TaskState::Running(CpuId(0)));
        assert_eq!(state_of(b), TaskState::Runnable);
        assert_eq!(current_task(), Some(a));
    }

    /// 提前 unpark 的 permit 必须由真实 park_current 快速路径消费，不能调度走 A。
    #[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
    #[test]
    fn park_consumes_pending_permit_without_switching() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();
        let mut cleanup = TaskCleanup::new();

        let owner = ready_component(b"sched_early_unpark_owner");
        let a = runnable_task(owner);
        cleanup.track(a);
        let b = runnable_task(owner);
        cleanup.track(b);
        crate::task::get_task_table()
            .lock()
            .transition(a, TaskState::Running(CpuId(0)))
            .unwrap();
        set_current(Some(a));
        containment::enter_task(a, owner);

        assert_eq!(unpark_task(owner, a), Ok(()));
        assert!(
            arch::fake::irq_enabled_for_test(),
            "nested unpark guards must restore the caller's IRQ state"
        );
        assert!(
            crate::task::get_task_table()
                .lock()
                .get(a)
                .unwrap()
                .park_pending()
        );
        assert_eq!(park_current(), Ok(()));
        assert!(
            arch::fake::irq_enabled_for_test(),
            "park's fast path must restore IRQ state before returning"
        );
        assert_eq!(state_of(a), TaskState::Running(CpuId(0)));
        assert_eq!(state_of(b), TaskState::Runnable);
        assert_eq!(current_task(), Some(a));
        assert!(
            !crate::task::get_task_table()
                .lock()
                .get(a)
                .unwrap()
                .park_pending()
        );
    }

    /// endpoint 是否已永久失效（`EndpointDead`）。
    fn endpoint_is_dead(endpoint: EndpointId) -> bool {
        let reg = registry::get_registry().lock();
        endpoint::get_endpoints().lock().lookup(
            &reg,
            endpoint,
            ContractId::from_raw(KCOMP_SCHEDULER_POLICY_CONTRACT),
            InterfaceAbi::from_raw(KCOMP_SCHEDULER_POLICY_ABI),
        ) == Err(EndpointError::EndpointDead)
    }

    // ------------------------------------------------------------------
    // 测试用 dispatcher（host fake 不执行组件入口体；模拟执行由
    // `containment::with_test_policy_dispatch` / `with_test_policy_panic` 安装）
    // ------------------------------------------------------------------

    /// frame 的 `args` = current TaskId（u32 LE；`KCOMP_SCHEDULER_NONE` = 无）。
    fn frame_current(frame: &KcompCallFrame) -> u32 {
        assert_eq!(
            frame.args_len,
            crate::generated::abi::KCOMP_SCHEDULER_ARGS_LEN,
            "CHOOSE_NEXT 的 args = current TaskId + CpuId"
        );
        // SAFETY: Core 构造的 frame；args_len = 8，读取前四字节，本调用期间有效。
        let bytes = unsafe { core::slice::from_raw_parts(frame.args, KCOMP_SCHEDULER_TASK_ID_LEN) };
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// frame 的 `input` = runnable 列表（逐个 u32 LE，非空）。
    fn frame_runnable_count(frame: &KcompCallFrame) -> usize {
        assert!(
            frame.input_len > 0 && frame.input_len.is_multiple_of(KCOMP_SCHEDULER_TASK_ID_LEN),
            "CHOOSE_NEXT 的 input = 非空 u32 列表"
        );
        frame.input_len / KCOMP_SCHEDULER_TASK_ID_LEN
    }

    /// 读 input 里第 `slot` 个 TaskId。
    fn frame_runnable_at(frame: &KcompCallFrame, slot: usize) -> u32 {
        let offset = slot * KCOMP_SCHEDULER_TASK_ID_LEN;
        // SAFETY: 调用方保证 slot < count；input_len = count * 4，本调用期间有效。
        let bytes = unsafe {
            core::slice::from_raw_parts(frame.input.add(offset), KCOMP_SCHEDULER_TASK_ID_LEN)
        };
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// 把提议写进 output。
    fn write_proposal(frame: &KcompCallFrame, task: u32) {
        assert_eq!(frame.output_len, KCOMP_SCHEDULER_TASK_ID_LEN);
        // SAFETY: output_len = 4（Core 构造），output 可写。
        unsafe {
            core::ptr::copy_nonoverlapping(
                task.to_le_bytes().as_ptr(),
                frame.output,
                KCOMP_SCHEDULER_TASK_ID_LEN,
            );
        }
    }

    /// 提议 runnable 首项（Core 会验证它落在列表内）。
    extern "C" fn first_runnable(
        _state: *mut (),
        _port: u32,
        method: u32,
        frame: *const KcompCallFrame,
    ) -> i32 {
        if method != KCOMP_SCHEDULER_METHOD_CHOOSE_NEXT {
            return Errno::EINVAL.code();
        }
        // SAFETY: Core 构造的 frame 在本调用期间有效。
        let frame = unsafe { &*frame };
        let first = frame_runnable_at(frame, 0);
        write_proposal(frame, first);
        0
    }

    /// 观察 frame 的 dispatcher：记录 current 与 runnable 数量，再提议首项。
    static SEEN_CURRENT: AtomicU32 = AtomicU32::new(u32::MAX);
    static SEEN_COUNT: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn first_runnable_observed(
        _state: *mut (),
        port: u32,
        method: u32,
        frame: *const KcompCallFrame,
    ) -> i32 {
        // SAFETY: Core 构造的 frame 在本调用期间有效。
        let frame = unsafe { &*frame };
        SEEN_CURRENT.store(frame_current(frame), Ordering::SeqCst);
        SEEN_COUNT.store(frame_runnable_count(frame), Ordering::SeqCst);
        first_runnable(
            ptr::null_mut(),
            port,
            method,
            frame as *const KcompCallFrame,
        )
    }

    /// 提议一个**不存在**的 TaskId(999)：Core 必须拒绝并隔离 provider。
    extern "C" fn propose_ghost(
        _state: *mut (),
        _port: u32,
        _method: u32,
        frame: *const KcompCallFrame,
    ) -> i32 {
        // SAFETY: Core 构造的 frame 在本调用期间有效。
        write_proposal(unsafe { &*frame }, 999);
        0
    }

    /// 返回非 0 方法状态：提议不可用（Core 必须隔离 provider + 回退）。
    extern "C" fn choose_fails(
        _state: *mut (),
        _port: u32,
        _method: u32,
        _frame: *const KcompCallFrame,
    ) -> i32 {
        Errno::EIO.code()
    }

    // ------------------------------------------------------------------
    // 选择（`kcore_sched_set_policy`）：校验、上下文、契约作用域
    // ------------------------------------------------------------------

    /// 验收：选择只接受**活**的 `scheduler.policy` endpoint，且 provider 必须真的
    /// 有 `kcomp_service_dispatch`；被拒绝的选择不写入配置。
    #[test]
    fn set_policy_rejects_dead_endpoint_and_provider_without_dispatcher() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        // (a) provider 没有 dispatcher：能力缺失 → ENOSYS，配置不变。
        let no_dispatch = ready_policy_provider(b"sched_select_no_dispatch", None, ptr::null_mut());
        let endpoint = publish_policy_endpoint(no_dispatch, 0);
        assert_eq!(set_policy(endpoint), Err(SchedError::NoDispatcher));
        assert_eq!(Errno::from(SchedError::NoDispatcher), Errno::ENOSYS);
        assert!(policy().lock().endpoint.is_none(), "拒绝的选择不得写入配置");

        // (b) endpoint 已永久失效（provider 停止 / 失败等价终态）→ ENOENT。
        let provider = ready_policy_provider(
            b"sched_select_dead",
            Some(first_runnable as *const () as usize),
            ptr::null_mut(),
        );
        let dead = publish_policy_endpoint(provider, 0);
        endpoint::get_endpoints().lock().invalidate_endpoint(dead);
        assert_eq!(
            set_policy(dead),
            Err(SchedError::PolicyEndpoint(EndpointError::EndpointDead))
        );
        assert_eq!(
            Errno::from(SchedError::PolicyEndpoint(EndpointError::EndpointDead)),
            Errno::ENOENT
        );
        assert!(policy().lock().endpoint.is_none());

        // (c) provider 离开 Ready（Failed）→ 同样拒绝（死 endpoint）。
        let failed = ready_policy_provider(
            b"sched_select_failed",
            Some(first_runnable as *const () as usize),
            ptr::null_mut(),
        );
        let failed_endpoint = publish_policy_endpoint(failed, 0);
        registry::get_registry().lock().mark_failed(failed).unwrap();
        assert_eq!(
            set_policy(failed_endpoint),
            Err(SchedError::PolicyEndpoint(EndpointError::EndpointDead))
        );
        assert!(policy().lock().endpoint.is_none());
        assert!(!policy().lock().retired, "从未选择过 = 未退役");
    }

    /// 验收：Isolated / Sandbox provider 不得被选为调度策略——策略回调在 Core
    /// 拥有的栈上、共享内核 AS 里执行（专用 PolicyCall 边界），provider 域内的
    /// dispatcher VA 在那里没有意义 → 显式拒绝（ENOTSUP），绝不静默降级。
    #[test]
    fn set_policy_rejects_providers_outside_kernel_native() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        let isolated = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"sched_isolated_policy",
                    crate::component::registry::test_support::test_loaded(
                        0,
                        Some(first_runnable as *const () as usize),
                    ),
                    ExecutionDomain::IsolatedNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        let endpoint = publish_policy_endpoint(isolated, 0);

        assert_eq!(
            set_policy(endpoint),
            Err(SchedError::PolicyUnsupportedDomain)
        );
        assert_eq!(
            Errno::from(SchedError::PolicyUnsupportedDomain),
            Errno::ENOTSUP
        );
        assert!(policy().lock().endpoint.is_none(), "拒绝的选择不得写入配置");
    }

    /// 验收：组合发现按 **(provider, port_name, contract)**，不是按名字——同名
    /// 端口发布别的契约不会被误选；IRQ / service-call 上下文内选择被拒（`-EINVAL`）。
    #[test]
    fn policy_discovery_is_contract_scoped_and_selection_is_context_gated() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        // 同名端口 + 不同契约：select_provider 绝不误选"名字像调度器"的 endpoint。
        let wrong = ready_policy_provider(
            b"sched_wrong_contract",
            Some(first_runnable as *const () as usize),
            ptr::null_mut(),
        );
        publish_policy_endpoint_at(
            wrong,
            0,
            ContractId::from_raw(0xDEAD_BEEF),
            InterfaceAbi::from_raw(0xDEAD_BEEF),
        );
        assert_eq!(
            select_provider(wrong),
            Err(SchedError::PolicyEndpoint(EndpointError::ContractMismatch))
        );
        assert!(policy().lock().endpoint.is_none());

        // 正确契约：发现 + 选择成功，配置只记 EndpointId。
        let good = ready_policy_provider(
            b"sched_good_contract",
            Some(first_runnable as *const () as usize),
            ptr::null_mut(),
        );
        let endpoint = publish_policy_endpoint(good, 0);
        assert_eq!(select_provider(good), Ok(()));
        assert_eq!(policy().lock().endpoint, Some(endpoint));

        // 上下文门禁：IRQ / service call 内不得改调度配置（-EINVAL）。
        containment::with_irq_scope(ComponentId::from_raw(0xBEEF), || {
            assert_eq!(set_policy(endpoint), Err(SchedError::InvalidTransition));
        });
        containment::with_test_service_boundary(good, endpoint, None, || {
            assert_eq!(set_policy(endpoint), Err(SchedError::InvalidTransition));
        });
        // 门禁恢复后选择照常（同一 endpoint 重新提交是幂等的）。
        assert_eq!(set_policy(endpoint), Ok(()));
    }

    // ------------------------------------------------------------------
    // 提议验证 / 失败处理 / 确定性回退
    // ------------------------------------------------------------------

    /// 验收：非法提议 → provider **endpoint-aware** 失败（逻辑死亡 + 全部
    /// endpoint 永久失效）+ 配置退役 + 确定性回退（id 序首项）。
    #[test]
    #[cfg(feature = "trace")]
    fn invalid_proposal_fails_provider_endpoint_aware_and_falls_back() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let _trace = crate::trace::test_support::GUARD.lock();
        init_world();

        // Given：已安装的策略永远提议不存在的 TaskId(999) + 一个活 owner 的任务。
        let (provider, endpoint) = install_policy(
            b"sched_bad_proposal",
            propose_ghost as *const () as usize,
            ptr::null_mut(),
        );
        let owner = ready_component(b"sched_bad_proposal_owner");
        let task = runnable_task(owner);
        let runnable = collect_runnable();
        assert!(runnable.contains(&task));

        // When：走真实 pick_next（锁内准备 → PolicyCall 边界 → Core 验证提议）。
        let picked = containment::with_test_policy_dispatch(propose_ghost, || pick_next(&runnable));

        // Then 1：真相未被改写——回退到 id 序首项，而不是不存在的 999。
        assert_eq!(picked, Ok(Some(task)));

        // Then 2：provider 逻辑死亡；endpoint 永久失效；inflight 已归还。
        let reg = registry::get_registry().lock();
        assert_eq!(reg.get(provider).unwrap().state, ComponentState::Failed);
        assert_eq!(reg.active_calls(provider), 0);
        drop(reg);
        assert!(endpoint_is_dead(endpoint));

        // Then 3：配置退役；后续调度仍是确定性回退，绝不退化成 NoPolicy。
        assert!(policy().lock().retired);
        assert_eq!(pick_next(&runnable), Ok(Some(task)));
        assert_eq!(pick_next(&runnable), Ok(Some(task)));

        // Then 4：真实事件序列：坏提议 → Core 拒绝 → provider 隔离。
        use crate::trace::{RejectReason, TraceEvent};
        crate::trace::test_support::assert_subsequence(
            &[
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

        // 清理。
        remove_task(task);
    }

    /// 验收：provider 返回非 0（契约违约）与非法提议同档——隔离 + 退役 + 回退。
    #[test]
    fn non_zero_policy_status_is_isolated_and_falls_back() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        let (provider, endpoint) = install_policy(
            b"sched_status_fail",
            choose_fails as *const () as usize,
            ptr::null_mut(),
        );
        let owner = ready_component(b"sched_status_fail_owner");
        let task = runnable_task(owner);
        let runnable = collect_runnable();

        let picked = containment::with_test_policy_dispatch(choose_fails, || pick_next(&runnable));
        assert_eq!(picked, Ok(Some(task)), "非 0 返回 → 确定性回退");
        assert!(registry::get_registry().lock().is_failed(provider));
        assert!(endpoint_is_dead(endpoint));
        assert!(policy().lock().retired);
        assert_eq!(pick_next(&runnable), Ok(Some(task)));

        remove_task(task);
    }

    /// 验收：**策略 panic 的完整收尾**——逃逸回挂起的调度帧（不继承让出 CPU 的
    /// 任务边界、不穿越它），provider 被标 `Failed`、全部 endpoint 永久失效、
    /// inflight 归还；配置退役后确定性回退生效（不是 `NoPolicy`）。
    ///
    /// host fake 不执行组件入口体，真实 escape 由 QEMU 证明；这里用
    /// `with_test_policy_panic` 走**同一**失败收尾路径（pick_next → prepare_policy
    /// → call_policy → Panicked → fail_component + 退役 + 回退）。
    #[test]
    fn policy_panic_returns_to_the_scheduler_frame_and_retires_to_fallback() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        // Given：已安装策略 + 一个正在运行（让出 CPU）的任务边界 + 另一个候选。
        let (provider, endpoint) = install_policy(
            b"sched_panic",
            first_runnable as *const () as usize,
            ptr::null_mut(),
        );
        let owner = ready_component(b"sched_panic_owner");
        let yielding = runnable_task(owner);
        let other = runnable_task(owner);
        set_current(Some(yielding));
        containment::enter_task(yielding, owner);

        // When：策略在边界内 panic（模拟；真实栈切换 / escape 由 QEMU 证明）。
        let runnable = collect_runnable();
        let picked = containment::with_test_policy_panic(|| pick_next(&runnable));

        // Then 1：Core 没被挂起——确定性回退（id 序首项）且提交前已验证 owner。
        assert_eq!(picked, Ok(Some(yielding)));

        // Then 2：provider 逻辑死亡 + endpoint 永久失效 + inflight 归还。
        let reg = registry::get_registry().lock();
        assert_eq!(reg.get(provider).unwrap().state, ComponentState::Failed);
        assert_eq!(reg.active_calls(provider), 0, "panic 路径必须归还 inflight");
        drop(reg);
        assert!(endpoint_is_dead(endpoint));

        // Then 3：panic **没有**继承 / 穿越让出 CPU 的任务边界——边界原样恢复。
        let info = containment::active_escape().expect("yielding task boundary intact");
        assert_eq!(info.task(), Some(yielding));
        assert_eq!(info.owner(), Some(owner));

        // Then 4：配置退役；下一次调度仍是确定性回退（不退化 NoPolicy），且不再
        //         调用任何组件（栈已保留、退役，绝不复用）。
        assert!(policy().lock().retired);
        assert!(policy().lock().stack.is_none(), "panic 的栈必须退役");
        assert_eq!(pick_next(&runnable), Ok(Some(yielding)));
        assert_eq!(pick_next(&runnable), Ok(Some(yielding)));

        // 清理。
        containment::enter_anchor();
        reset_cpu();
        remove_task(yielding);
        remove_task(other);
    }

    /// 验收：失败策略的回退任务恰好属于**刚失败的 provider** 时，回退在提交前
    /// 被验证拒绝（owner 已死）→ 回锚点，绝不把 CPU 交给已死实例的任务。
    #[test]
    fn failed_provider_owning_the_only_task_falls_back_to_the_anchor() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        // Given：provider 自己拥有唯一 Runnable 任务，策略 panic。
        let (provider, _endpoint) = install_policy(
            b"sched_owner_gate",
            first_runnable as *const () as usize,
            ptr::null_mut(),
        );
        let task = runnable_task(provider);
        let runnable = collect_runnable();
        assert!(runnable.contains(&task));

        // When
        let picked = containment::with_test_policy_panic(|| pick_next(&runnable));

        // Then：回退被提交前验证拒绝 → 回锚点；任务保持 Runnable、无 current。
        assert_eq!(picked, Ok(None), "回退任务属于刚失败的 provider → 回锚点");
        assert_eq!(run(), Ok(()));
        assert_eq!(
            state_of(task),
            TaskState::Runnable,
            "已死 owner 的任务不得被 dispatch"
        );
        assert_eq!(current_task(), None);

        remove_task(task);
    }

    // ------------------------------------------------------------------
    // PolicyCall 边界：调度 / 通用调用 / 嵌套创建 / 策略替换全部拒绝
    // ------------------------------------------------------------------

    /// 验收：policy 回调有界——`task run / yield / exit / create / start`、通用
    /// endpoint 调用、嵌套组件创建、策略替换全部拒绝（含嵌套边界之下）。
    #[test]
    fn policy_boundary_rejects_scheduling_endpoint_calls_and_nested_creation() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        let provider = ready_component(b"sched_gate_provider");
        let endpoint = EndpointId::from_raw(1);
        let nested_owner = ComponentId::from_raw(0x00C0_FFEE);

        containment::with_test_policy_boundary(provider, endpoint, || {
            // 调度操作：run / yield / exit 一律拒绝（调度帧正挂起）。
            assert_eq!(run(), Err(SchedError::InvalidTransition));
            assert_eq!(yield_current(), Err(SchedError::InvalidTransition));
            assert_eq!(exit_current(), Err(SchedError::InvalidTransition));
            // task create / start：同一门禁（`task::create_task` / `start_task`）。
            assert_eq!(
                crate::task::create_task(provider, ENTRY, ptr::null_mut()),
                Err(TaskError::InvalidTransition)
            );
            assert_eq!(
                crate::task::start_task(provider, TaskId::from_raw(0)),
                Err(TaskError::InvalidTransition)
            );
            // 通用 endpoint 调用：拒绝（Core 是 caller，但策略执行内不得再调组件）。
            let mut out_status = 0i32;
            assert_eq!(
                call::endpoint_call(
                    endpoint,
                    0,
                    ptr::null(),
                    0,
                    ptr::null(),
                    0,
                    ptr::null_mut(),
                    0,
                    &mut out_status,
                ),
                Err(CallError::InPolicyContext)
            );
            // 嵌套组件创建：拒绝。
            assert_eq!(
                crate::component::load::create_component(
                    b"sched_gate_missing",
                    &containment::KcompCreateArgs::empty(),
                    ExecutionDomain::KernelNative,
                ),
                Err(ComponentLoadError::InPolicyContext)
            );
            // 策略替换：拒绝（策略执行内不得改配置）。
            assert_eq!(set_policy(endpoint), Err(SchedError::InvalidTransition));

            // 藏在嵌套生命周期边界之下同样拒绝（祖先遍历，不是 top-guard-only）。
            containment::with_test_init_boundary(Some(nested_owner), || {
                assert_eq!(run(), Err(SchedError::InvalidTransition));
                assert_eq!(
                    call::endpoint_call(
                        endpoint,
                        0,
                        ptr::null(),
                        0,
                        ptr::null(),
                        0,
                        ptr::null_mut(),
                        0,
                        &mut out_status,
                    ),
                    Err(CallError::InPolicyContext)
                );
                assert_eq!(
                    crate::component::load::create_component(
                        b"sched_gate_missing",
                        &containment::KcompCreateArgs::empty(),
                        ExecutionDomain::KernelNative,
                    ),
                    Err(ComponentLoadError::InPolicyContext)
                );
            });
        });

        // 边界弹出：门禁恢复，且没有 stale 边界残留。
        assert!(!containment::policy_call_in_chain());
        assert!(!containment::scheduling_forbidden());
        containment::enter_anchor();
    }

    // ------------------------------------------------------------------
    // 无策略 / 锚点 / 任务耗尽
    // ------------------------------------------------------------------

    /// 纯逻辑：任务耗尽后 run() 不再切换（无锚点捕获、无 state 变更）。
    #[test]
    fn run_with_no_runnable_tasks_is_noop() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();
        // 空表：run 直接返回，不 panic、不切换。
        assert_eq!(run(), Ok(()));
    }

    /// **从未选择**策略时 Core 不猜、不退化成内置调度器：`run()` 返回
    /// `NoPolicy`，任务保持 Runnable、无 current、无状态推进。
    #[test]
    fn dispatch_without_policy_is_rejected_and_changes_nothing() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        // Given：活 owner + Runnable 任务，全程没有选择任何策略。
        let owner = ready_component(b"sched_no_policy_owner");
        let task = runnable_task(owner);
        assert!(collect_runnable().contains(&task));
        assert!(policy().lock().endpoint.is_none());

        // When
        let result = run();

        // Then
        assert_eq!(result, Err(SchedError::NoPolicy));
        assert_eq!(Errno::from(SchedError::NoPolicy), Errno::ENOTSUP);
        assert_eq!(
            state_of(task),
            TaskState::Runnable,
            "无 policy 不得推进任务状态"
        );
        assert_eq!(current_task(), None);

        remove_task(task);
    }

    /// `Failed` 组件拥有的 Runnable 任务既不进入候选，也不通过 commit 门禁；
    /// `run()` 安全返回 no-op（不挂起、不误调度）。
    #[test]
    fn failed_component_tasks_are_not_scheduled() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        // Given：一个 Ready 组件 + 一个 Runnable 任务（直接进全局 task 表）。
        let owner = ready_component(b"sched_failed_owner");
        let task = runnable_task(owner);

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
        remove_task(task);
    }

    /// `yield` / `exit` 只属于正在运行的任务：本 CPU 无 current 时两个入口都
    /// 返回 `NoCurrent`，且不产生任何状态/上下文副作用。
    #[test]
    fn yield_and_exit_without_current_task_are_rejected() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        // `init_world` 会释放上一个用例遗留的策略执行栈（全局堆）：持 memory GUARD。
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

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
        // `init_world` 会释放上一个用例遗留的策略执行栈（全局堆）：持 memory GUARD。
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

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

    /// 上下文种类门禁（祖先感知）：service-call 边界内的调度操作一律拒绝，
    /// 即使上面盖着嵌套的 init 边界——`Service → create → sched::run` 不能溜过。
    #[test]
    fn scheduler_operations_are_rejected_under_a_service_call_ancestor() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        // `init_world` 会释放上一个用例遗留的策略执行栈（全局堆）：持 memory GUARD。
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        containment::with_test_service_boundary(
            ComponentId::from_raw(0xB),
            EndpointId::from_raw(1),
            None,
            || {
                assert_eq!(run(), Err(SchedError::InvalidTransition));
                assert_eq!(yield_current(), Err(SchedError::InvalidTransition));
                assert_eq!(exit_current(), Err(SchedError::InvalidTransition));
                containment::with_test_init_boundary(Some(ComponentId::from_raw(0xC)), || {
                    assert_eq!(
                        run(),
                        Err(SchedError::InvalidTransition),
                        "a nested init boundary must not re-open the scheduler"
                    );
                    assert_eq!(yield_current(), Err(SchedError::InvalidTransition));
                    assert_eq!(exit_current(), Err(SchedError::InvalidTransition));
                });
            },
        );

        // 离开边界：上下文种类恢复（不再走拒绝路径，也绝不残留门禁）。
        assert!(!containment::scheduling_forbidden());
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
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

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
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

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
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

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

    /// commit 门禁对"表里不存在的任务"必须 fail-closed：未知 id 不能被当作
    /// 可运行（`None` 不是 `Some`——不会误调度幽灵任务，也不会 panic）。
    #[test]
    fn commit_gate_fails_closed_for_unknown_task() {
        let _sched = SCHED_TEST_LOCK.lock();
        // `init_world` 回到锚点边界（`enter_anchor` 写本 CPU 的 guard），
        // 必须持 BOUNDARY 锁。
        let _boundary = containment::test_boundary_lock();
        // `init_world` 还会释放上一个用例遗留的策略执行栈（全局堆）：持 memory GUARD。
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        assert!(!owner_still_runnable(TaskId::from_raw(0x0BAD_F00D)));
    }

    // ------------------------------------------------------------------
    // 完整 commit 链（host 侧模拟策略执行；真实执行由 QEMU 证明）
    // ------------------------------------------------------------------

    /// Abort 交接（**bookkeeping 部分**；栈抛弃 / 永不返回是 QEMU 契约）：
    /// 任务 panic 后先 `fail_component` 撤销 owner 的 authority，
    /// 再从存活 owner 的候选中选择后继、commit 死任务为 `Exited`——`ComponentState{Failed}`
    /// 事件先于 `TaskSwitch` 落账，Core 不被失败组件挂起。
    #[test]
    #[cfg(feature = "trace")]
    fn abort_handoff_commits_exit_fails_owner_and_switches_to_successor() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let _trace = crate::trace::test_support::GUARD.lock();
        init_world();

        // Given：已安装策略（提议 id 序首项）+ 活 owner 与活后继 owner；一个
        // Running 的"panicking"任务 + 另一个活 owner 的 Runnable 后继。
        let (provider, _endpoint) = install_policy(
            b"sched_abort_policy",
            first_runnable as *const () as usize,
            ptr::null_mut(),
        );
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
        let result = containment::with_test_policy_dispatch(first_runnable, || {
            schedule_next(Some(dying), Some(TaskState::Exited), Some((dying, owner)))
        });

        // Then 1：死任务 Exited、后继 Running、current = 后继。
        assert_eq!(result, Ok(()));
        assert_eq!(state_of(dying), TaskState::Exited);
        assert_eq!(state_of(successor), TaskState::Running(CpuId(0)));
        assert_eq!(current_task(), Some(successor));

        // Then 2：owner 逻辑死亡；后继 owner 不受影响。
        assert_eq!(
            registry::get_registry().lock().get(owner).unwrap().state,
            ComponentState::Failed
        );
        assert_eq!(
            registry::get_registry()
                .lock()
                .get(succ_owner)
                .unwrap()
                .state,
            ComponentState::Ready
        );
        // 策略 provider 保持 Ready（失败的是任务 owner，不是调度器）。
        assert_eq!(
            registry::get_registry().lock().get(provider).unwrap().state,
            ComponentState::Ready
        );

        // Then 3：事件顺序——先落 owner 的 Failed 账，再 TaskSwitch。
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
        reset_cpu();
    }

    #[test]
    fn aborting_policy_owner_excludes_its_siblings_and_uses_fallback() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();
        let (provider, _) = install_policy(
            b"sched_abort_provider",
            first_runnable as *const () as usize,
            ptr::null_mut(),
        );
        let healthy = ready_component(b"sched_abort_healthy");
        let mut cleanup = TaskCleanup::new();
        let dying = runnable_task(provider);
        let sibling = runnable_task(provider);
        let successor = runnable_task(healthy);
        for id in [dying, sibling, successor] {
            cleanup.track(id);
        }
        crate::task::get_task_table()
            .lock()
            .transition(dying, TaskState::Running(CpuId(0)))
            .unwrap();
        set_current(Some(dying));
        assert_eq!(
            schedule_next(
                Some(dying),
                Some(TaskState::Exited),
                Some((dying, provider))
            ),
            Ok(())
        );
        assert_eq!(current_task(), Some(successor));
        assert_eq!(state_of(dying), TaskState::Exited);
        assert_eq!(state_of(sibling), TaskState::Runnable);
        assert!(
            collect_runnable().is_empty(),
            "dead owner's sibling is excluded"
        );
        assert!(policy().lock().retired);
        containment::enter_anchor();
        reset_cpu();
    }

    /// 完整接受链（host 侧模拟策略执行）：run → yield → exit ×2。
    ///
    /// 断言四件事：
    /// 1. commit 真相：任务状态与 `current_task()` 的每次推进；
    /// 2. policy **wire 输入契约**：`args` = current（锚点为 `KCOMP_SCHEDULER_NONE`）、
    ///    `input` = Core 裁剪过的 runnable 列表（数量随候选收缩）；
    /// 3. 真实事件序列 `PolicyProposal → PolicyAccepted → TaskSwitch`（子序列匹配）；
    /// 4. 正常返回后策略栈留在槽里复用（配置保持 Armed）。
    #[test]
    #[cfg(feature = "trace")]
    fn run_yield_exit_commit_sequence_is_observable_in_truth_and_trace() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let _trace = crate::trace::test_support::GUARD.lock();
        init_world();

        // Given：已安装策略（提议 id 序首项）+ 一个活 owner + 两个 Runnable 任务。
        let (provider, _endpoint) = install_policy(
            b"sched_commit_policy",
            first_runnable_observed as *const () as usize,
            ptr::null_mut(),
        );
        let owner = ready_component(b"sched_commit_owner");
        let a = runnable_task(owner);
        let b = runnable_task(owner);
        assert!(a.raw() < b.raw(), "BTreeMap 迭代序 = id 升序");

        // When 1：锚点 → 调度。A 拿到 CPU（id 序首项）。
        assert_eq!(
            containment::with_test_policy_dispatch(first_runnable_observed, run),
            Ok(())
        );
        assert_eq!(
            SEEN_COUNT.load(Ordering::SeqCst),
            2,
            "policy 只看到两个活任务"
        );
        assert_eq!(
            SEEN_CURRENT.load(Ordering::SeqCst),
            KCOMP_SCHEDULER_NONE,
            "从锚点进入时无 current"
        );
        assert_eq!(state_of(a), TaskState::Running(CpuId(0)));
        assert_eq!(state_of(b), TaskState::Runnable);
        assert_eq!(current_task(), Some(a));

        // When 2：A 让出 → 只剩 B 是候选；policy 看到的 current 是 A。
        assert_eq!(
            containment::with_test_policy_dispatch(first_runnable_observed, yield_current),
            Ok(())
        );
        assert_eq!(SEEN_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(SEEN_CURRENT.load(Ordering::SeqCst), a.raw());
        assert_eq!(state_of(a), TaskState::Runnable);
        assert_eq!(state_of(b), TaskState::Running(CpuId(0)));
        assert_eq!(current_task(), Some(b));

        // When 3：B 退出 → A 接管（Runnable 里还有 A）。
        assert_eq!(
            containment::with_test_policy_dispatch(first_runnable_observed, exit_current),
            Ok(())
        );
        assert_eq!(SEEN_CURRENT.load(Ordering::SeqCst), b.raw());
        assert_eq!(state_of(b), TaskState::Exited);
        assert_eq!(state_of(a), TaskState::Running(CpuId(0)));
        assert_eq!(current_task(), Some(a));

        // When 4：A 退出 → 候选为空，policy 不再被咨询，控制权回锚点。
        assert_eq!(
            containment::with_test_policy_dispatch(first_runnable_observed, exit_current),
            Ok(())
        );
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

        // 正常返回：策略栈留在槽里复用（配置仍 Armed、未退役）。
        let slot = policy().lock();
        assert!(!slot.retired);
        assert!(slot.stack.is_some(), "正常返回的栈必须回到槽里复用");
        drop(slot);

        // 清理。
        remove_task(a);
        remove_task(b);
        reset_cpu();
    }

    /// 候选裁剪只放行活实例：死 owner 的 Runnable 任务既不进 policy 输入，
    /// 也不会被 commit；同一时刻活 owner 的任务照常被调度。
    #[test]
    fn dispatch_skips_dead_owner_and_runs_live_owner_task() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        // Given：一个活 owner 的任务 + 一个 Failed owner 的任务；策略提议首项。
        let (_provider, _endpoint) = install_policy(
            b"sched_mixed_policy",
            first_runnable_observed as *const () as usize,
            ptr::null_mut(),
        );
        let live_owner = ready_component(b"sched_mixed_live_owner");
        let dead_owner = ready_component(b"sched_mixed_dead_owner");
        let live = runnable_task(live_owner);
        let dead = runnable_task(dead_owner);
        registry::get_registry()
            .lock()
            .mark_failed(dead_owner)
            .unwrap();

        // When
        assert_eq!(
            containment::with_test_policy_dispatch(first_runnable_observed, run),
            Ok(())
        );

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
        assert_eq!(
            containment::with_test_policy_dispatch(first_runnable_observed, exit_current),
            Ok(())
        );
        remove_task(live);
        remove_task(dead);
        reset_cpu();
    }

    /// 性能基线（`make bench`）：**策略选择 + 提议 + Core 验证** 的成本。
    ///
    /// 只测到 `pick_next` 为止（host 侧策略执行是模拟的，不含真实栈切换 /
    /// context switch）。commit（`TaskTable::transition`）单独测；真正的
    /// context switch 必须在目标端测 —— host 的 `context_switch` 是 Fake no-op
    /// （见 docs/development/benchmark.md §6）。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_scheduler_propose_and_validate() {
        let _sched = SCHED_TEST_LOCK.lock();
        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        init_world();

        let (_provider, _endpoint) = install_policy(
            b"sched_bench_policy",
            first_runnable as *const () as usize,
            ptr::null_mut(),
        );
        let owner = ready_component(b"sched_bench_owner");
        let task = runnable_task(owner);
        let runnable = collect_runnable();

        crate::bench::report_environment();

        // 全路径：配置快照（锁）+ prepare_policy（锁 + begin_call）+ 模拟边界调用
        // + Core 验证 + 归还 inflight。
        containment::with_test_policy_dispatch(first_runnable, || {
            crate::bench::run("sched.pick_next", 1_000, || pick_next(&runnable).unwrap()).report();
        });

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
        remove_task(task);
    }
}
