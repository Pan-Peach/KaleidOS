//! Endpoint call —— `kcore_endpoint_call` 的 Core 实现（**服务调用执行边界**）。
//!
//! `kcore_endpoint_lookup` 在组合期把 `(provider, port_name, contract)` 解析成
//! opaque [`EndpointId`]；本模块把它变成一次真实调用：caller 身份门禁（无
//! principal / caller Failed → EPERM）→ IRQ 祖先门禁 → resolve（存活）→
//! re-entry 门禁 → `registry.begin_call`（Ready 门禁 + inflight 记账）→ 无锁按
//! **provider 执行域**分派 → `registry.finish_call`（正常 / panic / 故障三条路径
//! 都归还）。
//!
//! - **KernelNative provider** → [`containment::call_component_service`]：dispatcher
//!   跑在 Core 拥有的 per-call 32 KiB service stack、provider 自己的 principal
//!   之下；panic 时 provider 标 `Failed`、endpoint 永久失效、inflight 归还，
//!   caller 拿到 [`CallError::ProviderFailed`] 且 task 存活（绝不为 caller
//!   `abort_current_task`）。
//! - **IsolatedNative provider** → [`isolated_lifecycle::dispatch_service`]：在
//!   私有 AS / 实例栈上运行 dispatcher，故障后只结束 provider。
//! - **IsolatedNative caller** → `isolated_call`：验证并搬运扁平缓冲区，切到
//!   Core 栈与 Core root 分派，返回后恢复 caller AS 并写回 output / status。
//!   KernelNative caller 保留共享 Core 映射下的直接缓冲区路径。
//! - **Sandboxed caller/provider** → 显式拒绝，U-mode/ecall transport 未实现。
//!
//! **诚实边界**：这条 Gate 是 Core 拥有的机制，**不是对抗隔离边界**——Isolated
//! provider 与 Core 同特权级（S-mode，协作式），可以直接改 `satp` / 自己的映射；
//! 真正的强制边界是 U-mode（SandboxedNative，未实现）。ASID 恒 0 + 全量
//! `sfence.vma`。
//!
//! # 调度策略的专用路径（`sched::pick_next`）
//!
//! `SchedulerPolicy` 契约**不**经通用 `endpoint_call`：Core 自己是消费者，执行
//! 边界是 [`containment::call_component_policy`] + [`EscapeKind::PolicyCall`]；
//! 通用路径**拒绝** `scheduler.policy` 契约（[`CallError::ReservedContract`]），
//! policy 回调内（含嵌套边界之下）通用 endpoint 调用被拒
//! （[`CallError::InPolicyContext`]），policy 回调内不得创建组件 / 替换策略。
//!
//! # 锁纪律（不可动摇）
//!
//! 准备阶段可以同时持有 registry / endpoint 锁（**固定顺序**
//! `registry → endpoints`，无反向路径），但**任何锁都不得跨 provider
//! 调用**：dispatcher 地址、`instance_state`、`port` 在锁内拷贝进
//! [`DispatchTarget`]，全部 guard 释放后才执行组件代码。组件 dispatcher 在调用
//! 期间可以自由进入 Core（日志 / task / device…），"持锁调用组件" = 自死锁。
//! panic 收尾（`fail_component`）同样在边界返回之后、无锁状态下执行。
//!
//! # 传输状态 ≠ 方法状态
//!
//! [`endpoint_call`] 的返回值是 Core 的**传输状态**（`Ok` / `Err(CallError)`）；
//! provider 自己的 `i32` 返回写入 `*out_status`，**只在传输成功时有意义**。
//! provider 返回 `-EIO` 不是 Core 失败，Core 返回 `-ENOENT` 也不是 provider 的
//! 业务错误——两者永不混淆（`kcore_endpoint_call` 把 `Err` 翻成 `-Errno`）。
//!
//! # 存活解析（不重复校验 contract / abi）
//!
//! call ABI 不携带 contract / abi：`EndpointId` 是组合期经
//! [`EndpointRegistry::lookup`] / [`EndpointRegistry::discover`] 交付的 opaque
//! capability。调用只做**存活解析**（[`EndpointRegistry::resolve`]）：死 endpoint /
//! 死 owner 一律拒绝，绝不把调用重定向到新实例。
//!
//! Contract: `docs/modules/core/component.md`.

use crate::component::abi::InterfaceAbi;
use crate::component::containment::{self, CallOutcome, ServiceDispatch};
use crate::component::endpoint::{
    ContractId, EndpointError, EndpointId, EndpointRegistry, ExecutionDomain,
};
use crate::component::isolated_lifecycle;
use crate::component::load::ComponentLoadError;
use crate::component::registry::Registry;
use crate::component::{ComponentId, endpoint, registry};
use crate::generated::abi::{
    KCOMP_SCHEDULER_METHOD_CHOOSE_NEXT, KCOMP_SCHEDULER_POLICY_ABI,
    KCOMP_SCHEDULER_POLICY_CONTRACT, KcompCallFrame,
};
use crate::memory::MemoryLease;
use crate::resource::RequestContext;
use crate::task::TaskId;

/// endpoint call 的拒绝原因（内部强类型；ABI 翻译在 `errno.rs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    /// 无 ambient principal：不在任何 Core 管理的执行边界内 → `EPERM`。
    NoCaller,
    /// caller 已 `Failed`（逻辑死亡，不得发起新调用）→ `EPERM`。
    CallerFailed,
    /// frame 结构非法：`out_status` 为空，或长度非零但指针为空 → `EFAULT`。
    InvalidFrame,
    /// endpoint 存活解析失败（未发布 / 已死 / owner 消失）→ 见 [`EndpointError`]。
    Endpoint(EndpointError),
    /// provider 不在 `Ready`（停止 / 失败）或 inflight 计数溢出 → `EBUSY`。
    ProviderBusy,
    /// provider 的 loaded image 没有 `kcomp_service_dispatch`：组件不提供 endpoint 服务
    /// （能力缺失，不是故障）→ `ENOSYS`。
    NoDispatcher,
    /// provider 实例已在当前**同步调用链**上运行（它自己的 task / 外层 service
    /// call / 外层 init 或 exit）：这是重入，不是服务请求 → `EBUSY`。
    Reentrant,
    /// 当前调用链上存在 IRQ 归属作用域（即使藏在嵌套生命周期边界之下）：IRQ
    /// 回调是同步、不可 yield 的顶半部，不得发起通用服务调用 → `EINVAL`
    /// （与 IRQ 上下文中的调度拒绝同档）。
    InIrqContext,
    /// 当前调用链上存在 **policy-call** 边界（即使藏在嵌套生命周期边界之下）：
    /// 调度策略回调是 Core 调度 commit 路径的内部执行，调度帧正挂起，不得发起
    /// 通用 endpoint 调用 → `EINVAL`（与 IRQ 上下文同档）。
    InPolicyContext,
    /// endpoint 的契约是 Core **保留**的（`scheduler.policy`）：调度策略只能由
    /// Core 的调度路径经专用 PolicyCall 边界执行，组件不能把选中的调度算法当
    /// 普通服务跑 → `EPERM`。
    ReservedContract,
    /// Core 无法分配 service stack / transport buffer（`-ENOMEM`）：provider 入口从未执行，
    /// 传输失败，绝不写 `*out_status`。
    NoServiceStack,
    /// provider dispatcher 在 service 边界内 panic：provider 已被标记 `Failed`
    /// 且其全部 endpoint 永久失效；caller 存活且不变 → `EIO`。
    ProviderFailed,
    /// Caller 的执行后端不可用：Sandbox 尚未实现，或 Isolated 缺少真实
    /// 私有 AS / 活动切换现场 → `ENOTSUP`，绝不回退到 native 调用。
    UnsupportedCallerDomain,
    /// provider 的执行域没有**已实现**的 dispatch 机制（今天只有 KernelNative
    /// 与 IsolatedNative 有；SandboxedNative provider 未实现）→ `ENOTSUP`。
    /// 绝不静默降级成同域调用。
    UnsupportedProviderDomain,
}

impl From<EndpointError> for CallError {
    fn from(error: EndpointError) -> Self {
        Self::Endpoint(error)
    }
}

/// 锁内拷贝出的分派目标：锁外调用只碰这里的数据（+ 调用方内存）。
pub(crate) struct DispatchTarget {
    /// image 入口地址（**provider 域内**的 VA：KernelNative = Core AS；
    /// Isolated = 该实例私有 AS）。域决定谁把它变成可调用物。
    pub(crate) dispatcher: usize,
    pub(crate) instance_state: *mut (),
    pub(crate) port: u32,
    pub(crate) provider: ComponentId,
    /// provider 的执行域（Core 真相；机制选择的输入）。
    pub(crate) domain: ExecutionDomain,
}

/// 锁内准备：存活解析 → re-entry 门禁 → `begin_call` → 取 dispatcher。
///
/// 调用方必须在一个**作用域**里同时持有 registry / endpoint guard 并
/// 在离开作用域后（guard 释放后）才进入 [`containment::call_component_service`]。
fn prepare(
    components: &mut Registry,
    endpoints: &EndpointRegistry,
    id: EndpointId,
) -> Result<DispatchTarget, CallError> {
    // (1) 存活解析：死 endpoint / 死 owner 绝不派发（`resolve` 只查存活，
    //     contract / abi 已在组合期交付 id 之前校验）。
    let record = endpoints.resolve(components, id)?;
    if record.is_ipc_only() {
        return Err(CallError::NoDispatcher);
    }

    // (1b) 保留契约：`scheduler.policy` 不得经通用调用路径执行——调度策略只能
    //      由 Core 的调度路径经专用 PolicyCall 边界调用（`sched::pick_next`）。
    if record.contract == ContractId::from_raw(KCOMP_SCHEDULER_POLICY_CONTRACT) {
        return Err(CallError::ReservedContract);
    }

    // (2) re-entry 门禁：provider 已在当前同步链上（它自己的 task / 外层 service
    //     call / 外层 init 或 exit）→ 重入，不是服务请求。在 `begin_call` 之前
    //     拒绝：不产生需要归还的 inflight。
    if containment::provider_in_active_chain(record.owner) {
        return Err(CallError::Reentrant);
    }

    // (3) provider 自己的 loaded image / opaque state 在此刻拷贝（`resolve`
    //     刚校验过 owner 存在，故这里是纯读取；拷贝后不再借用其它记录）。
    let (dispatcher_opt, instance_state, domain) = {
        let Some(instance) = components.get(record.owner) else {
            return Err(CallError::Endpoint(EndpointError::ProviderNotFound));
        };
        (
            instance.loaded.service_dispatch,
            instance.instance_state,
            instance.execution_domain,
        )
    };

    // (4) inflight 记账门禁：只有 Ready provider 可以开始服务调用；拒绝
    //     （不在 Ready / 溢出 / 未知）统一映射成 EBUSY。此后任何提前返回
    //     都必须归还计数。
    // Isolated has one private entry stack per instance. Serialize entry across
    // CPUs as well as the same-CPU reentry check above; native providers have a
    // separate Core-owned stack for each call and retain their existing behavior.
    if domain == ExecutionDomain::IsolatedNative && components.active_calls(record.owner) != 0 {
        return Err(CallError::ProviderBusy);
    }
    components
        .begin_call(record.owner)
        .map_err(|_| CallError::ProviderBusy)?;

    // (5) **可选** dispatcher：缺失 = 组件不提供 endpoint 服务。
    let Some(dispatcher) = dispatcher_opt else {
        components.finish_call(record.owner);
        return Err(CallError::NoDispatcher);
    };

    Ok(DispatchTarget {
        // `service_dispatch` 只由 loader 写入（放段后解析 `STT_FUNC` 符号 +
        // 已分配 executable 段边界校验），组件无法伪造；组件常驻
        // （pinned-until-reboot），地址在调用期间有效。**域内 VA**：只有
        // `domain` 对应的 dispatch 路径可以把它变成可调用物。
        dispatcher,
        instance_state,
        port: record.port,
        provider: record.owner,
        domain,
    })
}

/// `kcore_endpoint_call` 的 Core 实现：解析 caller → [`dispatch`]。
///
/// # 结构校验（先于身份与解析）
///
/// `out_status` 必须可写（非空）；`args` / `input` / `output` 在长度非零时必须
/// 非空。Core **不解析** payload 字节（字段布局是契约的 SDK 侧职责），只保证交给
/// provider 的 `(ptr, len)` 不是明显非法的组合。
#[allow(clippy::too_many_arguments)]
pub fn endpoint_call(
    id: EndpointId,
    method: u32,
    args: *const u8,
    args_len: usize,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: usize,
    out_status: *mut i32,
) -> Result<(), CallError> {
    if out_status.is_null()
        || (args.is_null() && args_len != 0)
        || (input.is_null() && input_len != 0)
        || (output.is_null() && output_len != 0)
    {
        return Err(CallError::InvalidFrame);
    }
    let frame = KcompCallFrame {
        args,
        args_len,
        input,
        input_len,
        output,
        output_len,
    };
    let ambient = RequestContext::ambient();
    let caller = ambient.as_ref().map(|ctx| ctx.component);
    // caller task 只作为 service 边界的**执行来源**（provenance）；它不是授权，
    // 也不会改写 caller 的任务归属。
    let caller_task = ambient.as_ref().and_then(|ctx| ctx.task);
    if let Some(caller) = caller {
        if crate::component::is_failed(caller) {
            return Err(CallError::CallerFailed);
        }
        let domain = endpoint::instance_domain(&registry::get_registry().lock(), caller);
        if domain == ExecutionDomain::IsolatedNative {
            return super::isolated_call::call(caller, caller_task, id, method, &frame, out_status);
        }
    }
    dispatch(caller, caller_task, id, method, &frame, out_status)
}

/// 分派核心：`caller` 已由 [`endpoint_call`] 解析（`None` = 无 principal）。
///
/// caller / caller_task 作为显式参数：无 principal / 已 `Failed` 的 `EPERM` 门禁
/// 因此可以脱离进程级边界栈直接测试（`RequestContext` 的 fallback 链由
/// `resource::context` 自己的用例覆盖）。
pub(super) fn dispatch(
    caller: Option<ComponentId>,
    caller_task: Option<TaskId>,
    id: EndpointId,
    method: u32,
    frame: &KcompCallFrame,
    out_status: *mut i32,
) -> Result<(), CallError> {
    // (1) 身份门禁：无 principal / 已 Failed → EPERM（与其它 acquiring 入口一致）。
    let caller = caller.ok_or(CallError::NoCaller)?;
    if crate::component::is_failed(caller) {
        return Err(CallError::CallerFailed);
    }

    // Isolated callers reach this body only through the Core stack/root bridge.
    // Sandboxed outbound transport needs U-mode/ecall and remains unsupported.
    if endpoint::instance_domain(&registry::get_registry().lock(), caller)
        == ExecutionDomain::SandboxedNative
    {
        return Err(CallError::UnsupportedCallerDomain);
    }

    // (2) 祖先上下文门禁：IRQ 回调（即使藏在嵌套生命周期边界之下）不得发起
    //     通用服务调用——它是同步、不可 yield 的顶半部。
    if containment::irq_in_chain() {
        return Err(CallError::InIrqContext);
    }

    // (2b) policy-call 祖先门禁：策略回调内（含嵌套边界之下）不得发起通用
    //      endpoint 调用——调度帧正挂起，Core 没有可恢复的调用点。
    if containment::policy_call_in_chain() {
        return Err(CallError::InPolicyContext);
    }

    // (3) 锁内准备（含 re-entry 门禁）：三个 guard 在本块结束时全部释放——
    //     之后才允许执行组件代码。
    let target = {
        let mut components = registry::get_registry().lock();
        let endpoints = endpoint::get_endpoints().lock();
        prepare(&mut components, &endpoints, id)?
    };

    // (4) 无锁派发：按 **provider 执行域**选已实现的机制（Core 真相；
    //     `bind` 已在绑定时刻选定，这里只执行——绝不在 Core AS 里替 Isolated
    //     provider 跑它的 dispatcher）。
    match target.domain {
        // 同域 KernelNative：走 Core 控制的 service-call 执行边界
        // （per-call service stack + provider principal + panic containment）。
        ExecutionDomain::KernelNative => {
            // SAFETY: `dispatcher` 只由 loader 写入（放段后解析 `STT_FUNC` +
            // executable 段边界校验），组件无法伪造；image 常驻；KernelNative
            // 的入口 VA 在共享内核 AS 里就是可调用地址。
            let dispatcher: ServiceDispatch =
                unsafe { core::mem::transmute::<usize, ServiceDispatch>(target.dispatcher) };
            let outcome = containment::call_component_service(
                target.provider,
                id,
                caller_task,
                dispatcher,
                target.instance_state,
                target.port,
                method,
                frame,
            );
            complete_call(target.provider, outcome, out_status)
        }
        // 跨 AS：provider 在自己的私有 AS 里经跨 AS trampoline 执行；caller 帧在
        // 共享 Core 映射里 same VA → same PA，provider 原地读写（无拷贝）。
        ExecutionDomain::IsolatedNative => isolated_lifecycle::dispatch_service(
            target.provider,
            id,
            caller_task,
            target.dispatcher,
            target.instance_state,
            target.port,
            method,
            frame,
            out_status,
        ),
        // 没有已实现的 provider 机制（SandboxedNative 未实现）：显式拒绝，
        // 绝不静默降级成同域调用。
        ExecutionDomain::SandboxedNative => {
            registry::get_registry().lock().finish_call(target.provider);
            Err(CallError::UnsupportedProviderDomain)
        }
    }
}

// ---------------------------------------------------------------------------
// 调度策略调用（`sched::pick_next` 的专用执行路径；不是通用 service call）
// ---------------------------------------------------------------------------
//
// 与 [`dispatch`] 的三点不同（`docs/architecture/deployment.md` §3 "Gate" 的
// 调度特例，见 `containment::call_component_policy`）：
//
// 1. **Core 是 caller**：没有 caller principal / caller task——策略回调不是
//    "某个组件请求的服务"，而是 Core 调度 commit 路径的内部执行；
// 2. **契约固定**：endpoint 必须逐位匹配 `scheduler.policy` 的 contract + abi；
// 3. **无 re-entry 门禁**：策略 provider 自己的任务可以 yield（那时它的 task
//    帧挂起、代码不在执行），Core 仍需向它提议——重入只对通用 service call
//    成立。

/// 锁内拷贝出的**策略调用目标**：锁外调用只碰这里的数据（+ Core 构造的 frame）。
///
/// 全部字段在锁释放后仍然有效：`dispatch` 是 loader 校验 + 重定位后的 image 入口
/// （image 常驻 pinned-until-reboot）；`state` 是 provider 的 opaque instance state
/// （组件持有，Core 只传）；`port` 是 provider 发布时定义的不透明 dispatch token。
pub(crate) struct PolicyTarget {
    pub(crate) endpoint: EndpointId,
    pub(crate) owner: ComponentId,
    /// provider 的 opaque instance state（`kcomp_service_dispatch` 的第一个参数）。
    pub(crate) state: *mut (),
    pub(crate) port: u32,
    pub(crate) dispatch: ServiceDispatch,
}

/// 锁内准备一次策略调用：endpoint 校验（**contract + abi exact-match** + 存活）
/// → `begin_call` 记账 → 取 image dispatcher。
///
/// 锁序与 [`prepare`] 相同（`registry → endpoints`），两个 guard 在返回前
/// 全部释放；`begin_call` 之后的任何失败路径都归还 inflight。返回的目标交给
/// [`call_policy`] 在无锁状态下调用。
pub(crate) fn prepare_policy(id: EndpointId) -> Result<PolicyTarget, CallError> {
    let mut components = registry::get_registry().lock();
    let endpoints = endpoint::get_endpoints().lock();

    // (1) 契约 + abi + 存活：选择时已校验，每次调用重新核对（死 endpoint /
    //     provider 离开 Ready 一律拒绝；绝不重定向到新实例）。
    let record = endpoints.lookup(
        &components,
        id,
        ContractId::from_raw(KCOMP_SCHEDULER_POLICY_CONTRACT),
        InterfaceAbi::from_raw(KCOMP_SCHEDULER_POLICY_ABI),
    )?;

    // (2) provider 自己的 loaded image / opaque state（`lookup` 刚校验过 owner
    //     存在且 Ready）。
    let (dispatcher_opt, state) = {
        let Some(instance) = components.get(record.owner) else {
            return Err(CallError::Endpoint(EndpointError::ProviderNotFound));
        };
        (instance.loaded.service_dispatch, instance.instance_state)
    };

    // (3) inflight 记账门禁：只有 Ready provider 可以开始策略调用。
    components
        .begin_call(record.owner)
        .map_err(|_| CallError::ProviderBusy)?;

    // (4) **必需** dispatcher：策略 provider 必须提供 loaded image 级入口
    //     （选择时已校验，这里是每次调用的存活复验）。
    let Some(dispatcher) = dispatcher_opt else {
        components.finish_call(record.owner);
        return Err(CallError::NoDispatcher);
    };

    Ok(PolicyTarget {
        endpoint: id,
        owner: record.owner,
        state,
        port: record.port,
        // SAFETY: 同 `prepare`：`service_dispatch` 只由 loader 写入（放段后解析
        // `STT_FUNC` 符号 + 已执行段边界校验），组件无法伪造；组件常驻。
        dispatch: unsafe { core::mem::transmute::<usize, ServiceDispatch>(dispatcher) },
    })
}

/// 一次策略调用的结果：边界 outcome + 栈 lease。
pub(crate) struct PolicyCallOutcome {
    pub(crate) outcome: CallOutcome,
    /// `None` = provider panic，栈已保留并退役（绝不复用）。
    pub(crate) stack: Option<MemoryLease>,
}

/// 无锁调用已选择的策略（Core 是 caller；`method` 固定 `CHOOSE_NEXT`）。
///
/// 边界是 [`EscapeKind::PolicyCall`]：panic 时逃逸回**挂起的调度帧**（它仍持有
/// `IrqSaveGuard`），provider 被标记 `Failed`（逻辑死亡 + authority 回收 + 全部
/// endpoint 永久失效），inflight 归还；栈被保留、退役。**调用方负责**锁外构造
/// frame、以及失败后的确定性回退（见 `sched::pick_next`）。
pub(crate) fn call_policy(
    target: &PolicyTarget,
    frame: &KcompCallFrame,
    stack: MemoryLease,
) -> PolicyCallOutcome {
    let (outcome, stack) = containment::call_component_policy(
        target.owner,
        target.endpoint,
        target.dispatch,
        target.state,
        target.port,
        KCOMP_SCHEDULER_METHOD_CHOOSE_NEXT,
        frame,
        stack,
    );
    // 归还 inflight（正常 / panic / 无栈三条路径都归还）。
    registry::get_registry().lock().finish_call(target.owner);
    if outcome == CallOutcome::Panicked {
        // endpoint-aware 失败收尾（不是裸 `mark_failed`）：逻辑死亡 + 撤销
        // authority + provider 全部 endpoint 永久失效。
        crate::component::fail_component(target.owner, ComponentLoadError::PolicyPanicked);
    }
    PolicyCallOutcome { outcome, stack }
}

/// 服务边界返回后的收尾（[`dispatch`] 的尾段）。
///
/// 独立成函数，让 host 测试能直接驱动 panic / 无栈 / 正常三条分类（fake 后端不
/// 做真实上下文切换，provider 入口在 host 上不会被执行——真实执行由 QEMU 上
/// 导出 `kcomp_service_dispatch` 的组件证明）。
fn complete_call(
    provider: ComponentId,
    outcome: CallOutcome,
    out_status: *mut i32,
) -> Result<(), CallError> {
    match outcome {
        // provider 返回值 = 方法状态；只在传输成功时写 `*out_status`。
        CallOutcome::Returned(status) => {
            registry::get_registry().lock().finish_call(provider);
            // SAFETY: `out_status` 由调用方保证可写（C ABI 契约；入口已校验
            // 非空）；unaligned 写防未对齐 UB。
            unsafe { core::ptr::write_unaligned(out_status, status) };
            Ok(())
        }
        // provider panic：Core 提交 provider 的逻辑死亡 + 归还 inflight；caller
        // 的 task 保持存活、不变。
        CallOutcome::Panicked => {
            handle_provider_panic(provider);
            Err(CallError::ProviderFailed)
        }
        // 边界栈分配失败：Core 侧失败，provider 从未执行；不写 out_status。
        CallOutcome::NoStack => {
            registry::get_registry().lock().finish_call(provider);
            Err(CallError::NoServiceStack)
        }
    }
}

/// Provider dispatcher panic 的 Core 收尾：标记 provider `Failed`（逻辑死亡）、
/// 撤销它的 authority（设备 quarantine / IRQ route / DMA mapping / 接口解绑）并
/// 永久失效它的全部 endpoint，最后归还 `begin_call` 记下的 inflight。
///
/// **caller 保持存活且不变**：provider panic 在 service 边界被容纳（逃逸回
/// caller 的 Core 栈帧），绝不归因到 caller，也绝不调用 `abort_current_task`。
/// 这里是普通 Rust 代码——无 unwinding、不依赖 `Drop`。
///
/// 独立成函数，让 host 测试能脱离真实上下文切换直接驱动（fake 后端不执行组件
/// 入口，真实 provider panic 由 QEMU 证明）。
fn handle_provider_panic(provider: ComponentId) {
    crate::component::fail_component(provider, ComponentLoadError::ServicePanicked);
    registry::get_registry().lock().finish_call(provider);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::abi::{InterfaceAbi, InterfaceKind};
    use crate::component::containment;
    use crate::component::endpoint::{ContractId, EndpointError, ExecutionDomain};
    use crate::component::registry;
    use crate::errno::Errno;
    use crate::task::TaskId;
    use core::sync::atomic::{AtomicU32, Ordering};

    const CONTRACT: u64 = 0xCA11_0001;
    const ABI: u64 = 0xCA11_0002;
    const PORT: u32 = 7;
    /// 调用方任务 owner：不要求是注册实例（`deny_if_failed` 只拦已 Failed 的
    /// caller——身份不是权限，本层不发明额外门禁）。
    const CALLER: ComponentId = ComponentId::from_raw(0x00C0_FFEE);
    /// 嵌套生命周期边界的 owner：高位 id（真实 `declare` 序列到不了这里），
    /// 避免与并行用例在全局 registry 里留下的 `Failed` 记录串扰——`dispatch`
    /// 先查 `is_failed(caller)`，用 `ComponentId::from_raw(5)` 之类的小 id 会
    /// 依赖测试执行顺序（偶发 `CallerFailed`）。
    const NESTED_INIT: ComponentId = ComponentId::from_raw(0x00C0_FFEF);

    /// dispatcher 调用计数（"provider 从未被调用"的断言依据）。模块内测试由
    /// `containment::test_boundary_lock` 串行化，快照/比较是确定的。
    static DISPATCH_CALLS: AtomicU32 = AtomicU32::new(0);

    /// echo dispatcher 的观察记录（经 `instance_state` 传递）。
    #[derive(Default)]
    struct Seen {
        calls: u32,
        port: u32,
        method: u32,
        args: [u8; 3],
        input: [u8; 2],
    }

    /// 读 args / input、写 output、返回固定 status 的 dispatcher。
    extern "C" fn dispatch_echo(
        instance_state: *mut (),
        port: u32,
        method: u32,
        frame: *const KcompCallFrame,
    ) -> i32 {
        // SAFETY: 测试把 `instance_state` 登记为 `Seen`；`frame` 由 Core 构造，
        // 三个 (ptr, len) 在调用期间有效（本测试各自给足长度）。
        let seen = unsafe { &mut *(instance_state as *mut Seen) };
        let frame = unsafe { &*frame };
        seen.calls += 1;
        seen.port = port;
        seen.method = method;
        seen.args
            .copy_from_slice(unsafe { core::slice::from_raw_parts(frame.args, frame.args_len) });
        seen.input
            .copy_from_slice(unsafe { core::slice::from_raw_parts(frame.input, frame.input_len) });
        let written = [0xA5u8, 0x5A, 0xC3];
        // SAFETY: output 指向测试栈上的 3 字节 buffer（output_len = 3）。
        unsafe { core::ptr::copy_nonoverlapping(written.as_ptr(), frame.output, written.len()) };
        0x2A
    }

    /// 返回业务 errno 的 dispatcher（transport 必须仍是 0）。
    extern "C" fn dispatch_eio(
        _instance_state: *mut (),
        _port: u32,
        _method: u32,
        _frame: *const KcompCallFrame,
    ) -> i32 {
        Errno::EIO.code()
    }

    /// 计数 dispatcher（"从未派发"的证明）。
    extern "C" fn dispatch_counting(
        _instance_state: *mut (),
        _port: u32,
        _method: u32,
        _frame: *const KcompCallFrame,
    ) -> i32 {
        DISPATCH_CALLS.fetch_add(1, Ordering::SeqCst);
        0
    }

    /// 退化 frame（空 payload）：结构合法。
    const EMPTY_FRAME: KcompCallFrame = KcompCallFrame {
        args: core::ptr::null(),
        args_len: 0,
        input: core::ptr::null(),
        input_len: 0,
        output: core::ptr::null_mut(),
        output_len: 0,
    };

    /// 初始化全局真相，声明一个 Ready 组件（带可选 dispatcher）并记录
    /// `instance_state`，返回 provider id。
    fn ready_provider(
        name: &[u8],
        dispatcher: Option<usize>,
        instance_state: *mut (),
    ) -> ComponentId {
        ready_provider_in_domain(
            name,
            dispatcher,
            instance_state,
            ExecutionDomain::KernelNative,
        )
    }

    /// 同 [`ready_provider`]，但显式指定 provider 的执行域（部署真相）。
    fn ready_provider_in_domain(
        name: &[u8],
        dispatcher: Option<usize>,
        instance_state: *mut (),
        domain: ExecutionDomain,
    ) -> ComponentId {
        registry::init();
        endpoint::init();
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(
                name,
                crate::component::registry::test_support::test_loaded(0, dispatcher),
                domain,
            )
            .unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        reg.record_instance_state(id, instance_state).unwrap();
        id
    }

    /// 给 Ready provider 发布并提交一个 endpoint（staged → commit → discover）。
    fn publish(provider: ComponentId, port_name: &[u8]) -> EndpointId {
        let reg = registry::get_registry().lock();
        let mut endpoints = endpoint::get_endpoints().lock();
        endpoints
            .stage_publish(
                &reg,
                provider,
                port_name,
                ContractId::from_raw(CONTRACT),
                InterfaceKind::Device,
                InterfaceAbi::from_raw(ABI),
                PORT,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
        endpoints.commit_pending(&reg, provider).unwrap();
        endpoints
            .discover(&reg, provider, port_name, ContractId::from_raw(CONTRACT))
            .unwrap()
    }

    /// 建立 caller 边界（task 身份）并初始化全局真相；返回 heap guard。
    fn enter_caller(task: u32) {
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(task), CALLER);
    }

    // -- 1. 调用必须经 service 边界（host fake 不执行入口体） -------------------

    /// 验收：`endpoint_call` 走 `containment::call_component_service`，绝不直接调用
    /// dispatcher。host fake 上下文后端不执行组件入口体，所以 host 上能钉住的是：
    /// 边界被进入（dispatcher 从未被直接调用）、传输成功、inflight 归还。
    /// 真实的 frame 读写 / provider status 落位由 QEMU 上导出
    /// `kcomp_service_dispatch` 的组件证明（见模块文档的 Phase-1 限制）。
    #[test]
    fn endpoint_call_enters_the_service_boundary_and_balances_accounting() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let mut seen = Seen::default();
        let provider = ready_provider(
            b"call_echo_provider",
            Some(dispatch_echo as *const () as usize),
            (&mut seen as *mut Seen).cast(),
        );
        let endpoint = publish(provider, b"svc.echo");
        enter_caller(11);

        let args = [1u8, 2, 3];
        let input = [4u8, 5];
        let mut output = [0u8; 3];
        let mut out_status = 0i32;

        // When：调用 endpoint（method = 42，payload 全部非空）。
        let transport = endpoint_call(
            endpoint,
            42,
            args.as_ptr(),
            args.len(),
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
            &mut out_status,
        );

        // Then：传输成功、inflight 归还；fake 后端不执行入口体，所以 dispatcher
        // 一次都没被直接调用——"绝不绕过边界直接 invoke" 的回归锚点。
        assert_eq!(transport, Ok(()));
        assert_eq!(seen.calls, 0, "host fake 不执行组件入口；调用绝不绕过边界");
        assert_eq!(out_status, 0, "边界默认 outcome = 0（入口未执行）");
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);

        containment::enter_anchor();
    }

    // -- 2. 业务 errno 不冒充传输失败（边界收尾分类） ---------------------------

    /// 验收：`complete_call` 把 provider 返回值当**方法状态**写入 `*out_status`，
    /// 传输保持 `Ok`——绝不与 Core 失败混淆。host fake 不执行入口体，所以这里直接
    /// 驱动生产收尾函数（真实 provider 返回值由 QEMU 证明）。
    #[test]
    fn provider_errno_stays_in_out_status_not_transport() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_errno_provider",
            Some(dispatch_eio as *const () as usize),
            core::ptr::null_mut(),
        );

        // Given：一次已经记过 inflight 的调用。
        registry::get_registry()
            .lock()
            .begin_call(provider)
            .unwrap();
        let mut out_status = 0i32;

        // When：边界返回 provider 的业务 errno。
        let transport = complete_call(
            provider,
            CallOutcome::Returned(Errno::EIO.code()),
            &mut out_status,
        );

        // Then：传输仍是 0；负值只在 *out_status；inflight 已归还。
        assert_eq!(transport, Ok(()));
        assert_eq!(out_status, Errno::EIO.code());
        assert_ne!(out_status, 0, "provider 的 errno 不是传输状态的 0");
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
    }

    // -- 3. image 没有 dispatcher → ENOSYS，provider 不被调用 -------------------

    #[test]
    fn image_without_dispatcher_returns_enosys_and_never_dispatches() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        // Given：image 没有 `kcomp_service_dispatch`（组件不提供 endpoint 服务）。
        let provider = ready_provider(b"call_no_dispatch_provider", None, core::ptr::null_mut());
        let endpoint = publish(provider, b"svc.nodispatch");
        enter_caller(13);
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);

        let mut out_status = 0i32;
        // When / Then：文档化 errno = ENOSYS（能力缺失，不是 I/O 失败）。
        let error = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
        .unwrap_err();
        assert_eq!(error, CallError::NoDispatcher);
        assert_eq!(Errno::from(error), Errno::ENOSYS);
        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "provider 从未被调用"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(out_status, 0, "失败调用不写 out_status");

        containment::enter_anchor();
    }

    // -- 4. 死 endpoint → 拒绝，永不派发 ---------------------------------------

    #[test]
    fn dead_endpoint_is_rejected_and_never_dispatches() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_dead_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.dead");
        enter_caller(14);
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);

        // When：endpoint 被永久失效（provider 停止 / 失败的等价终态）。
        endpoint::get_endpoints()
            .lock()
            .invalidate_provider(provider);
        let mut out_status = 0i32;
        let error = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
        .unwrap_err();

        // Then：EndpointDead → ENOENT；dispatcher 未被调用；inflight 未泄漏。
        assert_eq!(error, CallError::Endpoint(EndpointError::EndpointDead));
        assert_eq!(Errno::from(error), Errno::ENOENT);
        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "死 endpoint 绝不派发"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);

        containment::enter_anchor();
    }

    // -- 5. 无 principal / caller 已 Failed → EPERM ---------------------------

    #[test]
    fn no_or_failed_caller_is_rejected_with_eperm() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_caller_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.caller");
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);
        let mut out_status = 0i32;

        // (a) 无 principal → NoCaller（EPERM）。caller 是显式参数：本断言不依赖
        //     `RequestContext` 的 fallback 链（无边界锚点的进程级状态），确定。
        let error = dispatch(None, None, endpoint, 0, &EMPTY_FRAME, &mut out_status).unwrap_err();
        assert_eq!(error, CallError::NoCaller);
        assert_eq!(Errno::from(error), Errno::EPERM);

        // (b) caller 已 Failed（逻辑死亡）→ CallerFailed（同一 EPERM 档位）。
        let failed = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"call_failed_caller",
                    crate::component::registry::test_support::test_loaded(0, None),
                    ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.mark_failed(id).unwrap();
            id
        };
        let error = dispatch(
            Some(failed),
            None,
            endpoint,
            0,
            &EMPTY_FRAME,
            &mut out_status,
        )
        .unwrap_err();
        assert_eq!(error, CallError::CallerFailed);
        assert_eq!(Errno::from(error), Errno::EPERM);

        // Then：两条门禁都在 provider 之前；inflight 未被触碰。
        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "provider 从未被调用"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(out_status, 0, "失败调用不写 out_status");
    }

    /// Sandboxed caller 尚无 U-mode/ecall transport，必须在 dispatch 前拒绝。
    ///
    /// 门禁在 provider 解析 / inflight 记账**之前**，与其它 caller 门禁同序。
    #[test]
    fn sandboxed_caller_is_rejected_before_any_dispatch() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_sandboxed_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.sandboxed");
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);

        // Given：一个 Sandboxed 实例（Starting = 活实例，身份有效）。
        let sandboxed = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"call_sandboxed_caller",
                    crate::component::registry::test_support::test_loaded(0, None),
                    ExecutionDomain::SandboxedNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };

        // When：它调用一个真实存活、Ready 的 provider。
        let mut out_status = 0i32;
        let error = dispatch(
            Some(sandboxed),
            None,
            endpoint,
            0,
            &EMPTY_FRAME,
            &mut out_status,
        )
        .unwrap_err();

        // Then：`UnsupportedCallerDomain`（ENOTSUP）；provider 从未被调用、
        // inflight 未被触碰、out_status 不写（传输失败 ≠ 方法状态）。
        assert_eq!(error, CallError::UnsupportedCallerDomain);
        assert_eq!(Errno::from(error), Errno::ENOTSUP);
        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "provider 从未被调用"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(out_status, 0, "失败调用不写 out_status");
    }

    // -- 6. 非法 frame / 空 out_status → EFAULT，永不派发 -----------------------

    #[test]
    fn invalid_frame_and_null_out_status_are_rejected_and_never_dispatch() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_frame_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.frame");
        enter_caller(15);
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);
        let mut out_status = 0i32;
        let args = [0u8; 1];

        // out_status 为空 → EFAULT。
        let error = endpoint_call(
            endpoint,
            0,
            args.as_ptr(),
            1,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            core::ptr::null_mut(),
        )
        .unwrap_err();
        assert_eq!(error, CallError::InvalidFrame);
        assert_eq!(Errno::from(error), Errno::EFAULT);

        // 长度非零但指针为空（args / output）→ EFAULT：不把非法 (ptr, len) 交给 provider。
        let error = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            1,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
        .unwrap_err();
        assert_eq!(error, CallError::InvalidFrame);
        let error = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            1,
            &mut out_status,
        )
        .unwrap_err();
        assert_eq!(error, CallError::InvalidFrame);

        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "结构非法的调用绝不派发"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(out_status, 0, "失败调用不写 out_status");

        containment::enter_anchor();
    }

    // -- 7. frame 布局（6 指针宽）---------------------------------------------
    //
    // 生成物 `generated/abi.rs` 的 `const _` 断言在编译期覆盖 size / align /
    // offset（`make abi-check` 守住生成物新鲜度）；host 侧不再重复钉。

    // -- 8. re-entry：provider 已在当前同步链上 → EBUSY --------------------------

    /// 验收：provider 自己的 task、以及外层 service call，都让再次调用同一
    /// provider 被拒为 `Reentrant`（EBUSY）；调用**另一个** provider 允许；
    /// 被拒的调用不触碰 inflight / dispatcher。
    #[test]
    fn provider_already_in_the_chain_is_rejected_as_reentrant() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_reentry_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.reentry");
        let other = ready_provider(
            b"call_reentry_other",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let other_endpoint = publish(other, b"svc.other");
        enter_caller(21);
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);
        let mut out_status = 0i32;

        // (a) provider 自己的 task 调用自己的 endpoint → Reentrant（EBUSY）。
        containment::enter_task(TaskId::from_raw(22), provider);
        let error = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
        .unwrap_err();
        assert_eq!(error, CallError::Reentrant);
        assert_eq!(Errno::from(error), Errno::EBUSY);

        // (b) 外层 service call 里再调用同一 provider → Reentrant；
        // (c) 调用另一个 provider 允许（重入只针对链上已有的实例）。
        containment::with_test_service_boundary(
            provider,
            endpoint,
            Some(TaskId::from_raw(22)),
            || {
                let error = endpoint_call(
                    endpoint,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null_mut(),
                    0,
                    &mut out_status,
                )
                .unwrap_err();
                assert_eq!(error, CallError::Reentrant);

                assert_eq!(
                    endpoint_call(
                        other_endpoint,
                        0,
                        core::ptr::null(),
                        0,
                        core::ptr::null(),
                        0,
                        core::ptr::null_mut(),
                        0,
                        &mut out_status,
                    ),
                    Ok(()),
                    "a different provider is not re-entry"
                );
            },
        );

        // Then：provider 从未被调用；被拒调用不泄漏 inflight、不写 out_status。
        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "provider 从未被调用"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(registry::get_registry().lock().active_calls(other), 0);
        assert_eq!(out_status, 0);

        containment::enter_anchor();
    }

    // -- 9. 祖先上下文门禁：IRQ scope 之下（含嵌套）拒绝服务调用 -----------------

    /// 验收：IRQ 回调链上（即使藏在嵌套 init 边界下面）不得发起通用服务调用
    /// → `InIrqContext`（EINVAL），provider 从未被调用。
    #[test]
    fn endpoint_call_from_an_irq_ancestor_is_rejected() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_irq_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.irq");
        enter_caller(23);
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);
        let mut out_status = 0i32;

        containment::with_irq_scope(ComponentId::from_raw(0xBEEF), || {
            let error = endpoint_call(
                endpoint,
                0,
                core::ptr::null(),
                0,
                core::ptr::null(),
                0,
                core::ptr::null_mut(),
                0,
                &mut out_status,
            )
            .unwrap_err();
            assert_eq!(error, CallError::InIrqContext);
            assert_eq!(Errno::from(error), Errno::EINVAL);

            // 藏在嵌套生命周期边界之下同样拒绝（top-guard-only 检查会漏掉）。
            containment::with_test_init_boundary(Some(NESTED_INIT), || {
                let error = endpoint_call(
                    endpoint,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null_mut(),
                    0,
                    &mut out_status,
                )
                .unwrap_err();
                assert_eq!(error, CallError::InIrqContext);
            });
        });

        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "IRQ 上下文里的调用绝不派发"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(out_status, 0, "失败调用不写 out_status");

        containment::enter_anchor();
    }

    // -- 10. provider panic 收尾（无上下文切换） --------------------------------

    /// 验收：provider panic 的 Core 收尾——provider → `Failed`、它的全部 endpoint
    /// 永久失效（sibling 的 endpoint 仍 Live）、inflight 归还。
    #[test]
    fn provider_panic_fails_provider_invalidates_endpoints_and_balances_inflight() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        crate::resource::init();
        let provider = ready_provider(
            b"call_panic_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.panic");
        let sibling = ready_provider(
            b"call_panic_sibling",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let sibling_endpoint = publish(sibling, b"svc.ok");

        // Given：一次在飞的调用（begin_call 已记账）。
        registry::get_registry()
            .lock()
            .begin_call(provider)
            .unwrap();
        assert_eq!(registry::get_registry().lock().active_calls(provider), 1);

        // When：dispatcher panic 的收尾。
        handle_provider_panic(provider);

        // Then 1：provider 逻辑死亡；inflight 归还。
        let reg = registry::get_registry().lock();
        assert_eq!(
            reg.get(provider).unwrap().state,
            crate::component::ComponentState::Failed
        );
        assert_eq!(reg.active_calls(provider), 0);
        // Then 2：provider 的全部 endpoint 永久失效；sibling 完全不受影响。
        let eps = endpoint::get_endpoints().lock();
        assert_eq!(
            eps.lookup(
                &reg,
                endpoint,
                ContractId::from_raw(CONTRACT),
                InterfaceAbi::from_raw(ABI)
            ),
            Err(EndpointError::EndpointDead)
        );
        assert_eq!(
            eps.lookup(
                &reg,
                sibling_endpoint,
                ContractId::from_raw(CONTRACT),
                InterfaceAbi::from_raw(ABI)
            )
            .unwrap()
            .state,
            crate::component::endpoint::EndpointState::Live
        );
        drop(eps);
        // Then 3：sibling 仍 Ready（失败只影响被隔离的 provider）。
        assert_eq!(
            reg.get(sibling).unwrap().state,
            crate::component::ComponentState::Ready
        );
    }

    /// 验收：边界返回 `Panicked` 时传输错误是 `ProviderFailed`（EIO），且
    /// **caller 的 task 边界原样存活**——provider panic 绝不转成 caller 的死亡，
    /// 也不写 `*out_status`。
    #[test]
    fn panicked_boundary_returns_provider_failed_and_leaves_caller_alive() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        crate::resource::init();
        let provider = ready_provider(
            b"call_panicked_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.panicked");
        enter_caller(24);
        registry::get_registry()
            .lock()
            .begin_call(provider)
            .unwrap();
        let mut out_status = 0i32;

        // When：边界报告 provider panic。
        let error = complete_call(provider, CallOutcome::Panicked, &mut out_status).unwrap_err();

        // Then：传输错误 = ProviderFailed（EIO）；不写 out_status；inflight 归还。
        assert_eq!(error, CallError::ProviderFailed);
        assert_eq!(Errno::from(error), Errno::EIO);
        assert_eq!(out_status, 0, "panic 路径不写 out_status");
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        // caller 边界仍在：provider 的逻辑死亡没有波及 caller 的任务归属。
        let ambient = crate::resource::RequestContext::ambient().expect("caller boundary intact");
        assert_eq!(ambient.component, CALLER);
        assert_eq!(ambient.task, Some(TaskId::from_raw(24)));
        // provider 的 endpoint 永久失效。
        let reg = registry::get_registry().lock();
        assert_eq!(
            endpoint::get_endpoints().lock().lookup(
                &reg,
                endpoint,
                ContractId::from_raw(CONTRACT),
                InterfaceAbi::from_raw(ABI)
            ),
            Err(EndpointError::EndpointDead)
        );
        drop(reg);
        containment::enter_anchor();
    }

    /// 验收：service stack 分配失败是 **Core 侧**传输失败（`NoServiceStack` →
    /// ENOMEM），provider 从未执行，绝不冒充 provider 的方法状态。
    #[test]
    fn missing_service_stack_is_a_transport_failure_not_provider_status() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_nostack_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        registry::get_registry()
            .lock()
            .begin_call(provider)
            .unwrap();
        let mut out_status = 0i32;

        let error = complete_call(provider, CallOutcome::NoStack, &mut out_status).unwrap_err();
        assert_eq!(error, CallError::NoServiceStack);
        assert_eq!(Errno::from(error), Errno::ENOMEM);
        assert_eq!(out_status, 0, "provider 从未执行");
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
    }

    // -- 11. Gate 选中的绑定仍然可用 ---------------------------------------------

    /// 交叉组合（caller 域 ≠ provider 域）在 bind 上被选为 **Gate**：binding 只携带
    /// opaque EndpointId（不携带裸 function table）；该 endpoint 仍经
    /// `kcore_endpoint_call` 正常派发。host fake 不执行入口体，所以这里钉住的是
    /// "传输成功 + inflight 归还"——真实入口执行由 QEMU 上的真实组件证明。
    #[test]
    fn gate_selected_endpoint_still_dispatches_through_endpoint_call() {
        use crate::component::endpoint::Mechanism;

        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_gate_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );
        let endpoint = publish(provider, b"svc.gate");

        // Given：两端执行域不同（Isolated caller → KernelNative provider）→ Gate。
        let bound = {
            let reg = registry::get_registry().lock();
            endpoint::get_endpoints()
                .lock()
                .bind(
                    &reg,
                    endpoint,
                    ContractId::from_raw(CONTRACT),
                    InterfaceAbi::from_raw(ABI),
                    ExecutionDomain::IsolatedNative,
                )
                .unwrap()
        };
        assert_eq!(bound.mechanism, Mechanism::Gate);
        assert_eq!(
            bound.record.port, PORT,
            "Gate 经 port + kcomp_service_dispatch 分派"
        );

        // When：caller 经 Core call gate 调用同一 endpoint。
        enter_caller(31);
        let mut out_status = 0i32;
        let transport = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        );

        // Then：传输成功、inflight 归还（provider 入口由 host fake 记账，不真实执行）。
        assert_eq!(transport, Ok(()));
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);

        containment::enter_anchor();
    }

    // -- 11b. 跨域 Gate：KernelNative caller → Isolated provider --------------------

    /// 验收（host 面）：Isolated provider 的 dispatch 需要**真实私有 AS backend**；
    /// host / 无 backend 构建显式拒绝（`UnsupportedProviderDomain` → ENOTSUP），
    /// **绝不**在共享内核 AS 里替它执行 dispatcher，也不泄漏 inflight / out_status。
    ///
    /// 真实的跨 AS 执行（caller 帧直接交付 + trampoline + 故障 containment）由 QEMU ArchTest
    /// `isolated-service*` 用真实 `.kcomp` 证明。
    #[test]
    fn isolated_provider_is_rejected_on_a_build_without_a_private_address_space() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider_in_domain(
            b"call_isolated_gate_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
            ExecutionDomain::IsolatedNative,
        );
        let endpoint = publish(provider, b"svc.isolated.gate");
        enter_caller(51);
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);
        let mut out_status = 0i32;

        let error = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
        .unwrap_err();

        assert_eq!(error, CallError::UnsupportedProviderDomain);
        assert_eq!(Errno::from(error), Errno::ENOTSUP);
        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "provider 从未被调用"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(out_status, 0, "失败调用不写 out_status");

        containment::enter_anchor();
    }

    #[test]
    fn isolated_provider_cannot_enter_its_private_stack_concurrently() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider_in_domain(
            b"call_isolated_busy_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
            ExecutionDomain::IsolatedNative,
        );
        let endpoint = publish(provider, b"svc.isolated.busy");
        // A call running on another CPU already owns the sole instance stack.
        registry::get_registry()
            .lock()
            .begin_call(provider)
            .unwrap();
        enter_caller(52);
        let mut status = 123;
        assert_eq!(
            endpoint_call(
                endpoint,
                0,
                core::ptr::null(),
                0,
                core::ptr::null(),
                0,
                core::ptr::null_mut(),
                0,
                &mut status
            ),
            Err(CallError::ProviderBusy)
        );
        assert_eq!(status, 123);
        assert_eq!(registry::get_registry().lock().active_calls(provider), 1);
        registry::get_registry().lock().finish_call(provider);
        containment::enter_anchor();
    }

    /// 验收（host 面）：Sandboxed provider 没有已实现的 dispatch 机制 →
    /// 显式拒绝（ENOTSUP），**绝不静默降级**成同域调用。
    #[test]
    fn sandboxed_provider_is_rejected_explicitly() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider_in_domain(
            b"call_sandbox_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
            ExecutionDomain::SandboxedNative,
        );
        let endpoint = publish(provider, b"svc.sandbox");
        enter_caller(53);
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);
        let mut out_status = 0i32;

        let error = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
        .unwrap_err();

        assert_eq!(error, CallError::UnsupportedProviderDomain);
        assert_eq!(Errno::from(error), Errno::ENOTSUP);
        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "provider 从未被调用"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(out_status, 0);

        containment::enter_anchor();
    }

    // -- 12. 保留契约：调度策略不得经通用调用路径执行 -----------------------------
    /// 验收：`scheduler.policy` 契约的 endpoint 经通用 `kcore_endpoint_call` 被拒
    /// （`ReservedContract` → EPERM）——调度策略只能由 Core 的调度路径经专用
    /// PolicyCall 边界执行，组件不能把选中的调度算法当普通服务跑。
    #[test]
    fn scheduler_policy_contract_is_reserved_from_generic_calls() {
        let _serial = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let provider = ready_provider(
            b"call_reserved_provider",
            Some(dispatch_counting as *const () as usize),
            core::ptr::null_mut(),
        );

        // Given：一个发布 `scheduler.policy` 契约的 endpoint（contract / abi 用
        // 生成常量，与 Core 的保留判定同源）。
        let contract = ContractId::from_raw(KCOMP_SCHEDULER_POLICY_CONTRACT);
        let abi = InterfaceAbi::from_raw(KCOMP_SCHEDULER_POLICY_ABI);
        let endpoint = {
            let reg = registry::get_registry().lock();
            let mut eps = endpoint::get_endpoints().lock();
            eps.stage_publish(
                &reg,
                provider,
                b"scheduler.policy",
                contract,
                InterfaceKind::Policy,
                abi,
                0,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
            eps.commit_pending(&reg, provider).unwrap();
            eps.discover(&reg, provider, b"scheduler.policy", contract)
                .unwrap()
        };
        enter_caller(41);
        let before = DISPATCH_CALLS.load(Ordering::SeqCst);
        let mut out_status = 0i32;

        // When：caller 经通用调用路径调用它。
        let error = endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
        .unwrap_err();

        // Then：保留契约拒绝（EPERM）；provider 从未被调用、inflight 未泄漏。
        assert_eq!(error, CallError::ReservedContract);
        assert_eq!(Errno::from(error), Errno::EPERM);
        assert_eq!(
            DISPATCH_CALLS.load(Ordering::SeqCst),
            before,
            "provider 从未被调用"
        );
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);
        assert_eq!(out_status, 0, "失败调用不写 out_status");

        containment::enter_anchor();
    }
}
