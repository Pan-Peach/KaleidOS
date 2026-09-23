//! Endpoint call —— `kcore_endpoint_call` 的 Core 实现（**服务调用执行边界**）。
//!
//! # 定位
//!
//! 组合期用 `kcore_endpoint_lookup` 把 `(provider, port_name, contract)` 解析成
//! opaque [`EndpointId`]；本模块把它变成**一次真实调用**：
//!
//! ```text
//! caller（最内层活动执行边界，RequestContext::ambient）
//!   → caller 身份门禁（无 principal / caller Failed → EPERM）
//!   → IRQ 祖先门禁（链上任何 Irq scope，含嵌套之下的 → EINVAL）
//!   → resolve endpoint（存活：endpoint Live + owner 存在且 Ready）
//!   → re-entry 门禁（provider 已在当前同步链上 → EBUSY）
//!   → registry.begin_call(provider)（Ready 门禁 + inflight 记账）
//!   → 取 provider image 的可选 kcomp_service_dispatch + instance_state + port
//!   → 【无锁】containment::call_component_service(...)
//!        （per-call service stack + provider principal + panic containment）
//!   → registry.finish_call(provider)（正常 / panic / 无栈三条路径都归还）
//!   → 传输状态：Ok / Err(CallError)；provider 返回值只在 Ok 时写 `*out_status`
//! ```
//!
//! # 执行边界（本阶段落地）
//!
//! provider 的 dispatcher 跑在 **Core 拥有的 per-call 32 KiB service stack** 上，
//! 处于 provider 自己的 principal 之下（[`containment::call_component_service`]）：
//!
//! - **principal 切换**：dispatcher 内 `RequestContext::ambient()` 解析为
//!   provider（不再是 caller）；caller 的 task 只作为**执行来源**传递，不是
//!   授权。`ambient_init()` 在边界内为 `None`（service call 不得发布）。
//! - **panic containment**：dispatcher panic 时逃逸回 caller 的 Core 栈帧，
//!   Core 把 provider 标 `Failed`、撤销其 authority 并永久失效它的全部
//!   endpoint、归还 inflight，向 caller 返回 [`CallError::ProviderFailed`]——
//!   **caller 的 task 存活且不变**（绝不为 caller 调用 `abort_current_task`）。
//! - **re-entry 拒绝**：provider 已在当前同步链上（它自己的 task / 外层 service
//!   call / 外层 init 或 exit）→ [`CallError::Reentrant`]；调度锚点不被穿越。
//! - **祖先上下文门禁**：链上任何 IRQ scope（即使藏在嵌套生命周期边界下面）
//!   都拒绝通用服务调用 → [`CallError::InIrqContext`]。
//! - **调度门禁**：service 边界内（含嵌套 init 之下）`sched::run` /
//!   `yield_current` / `exit_current` / task 创建一律拒绝
//!   （`containment::scheduling_forbidden`）——provider 没有调度可见的任务。
//!
//! # 传输状态 ≠ 方法状态
//!
//! [`endpoint_call`] 的返回值是 Core 的**传输状态**（`Ok` / `Err(CallError)`）；
//! provider 自己的 `i32` 返回写入 `*out_status`，**只在传输成功时有意义**。
//! provider 返回 `-EIO` 不是 Core 失败，Core 返回 `-ENOENT` 也不是 provider 的
//! 业务错误——两者永不混淆（`kcore_endpoint_call` 把 `Err` 翻成 `-Errno`）。
//!
//! # 锁纪律（不可动摇）
//!
//! 准备阶段可以同时持有 registry / endpoint / image 锁（**固定顺序**
//! `registry → endpoints → images`，无反向路径），但**任何锁都不得跨 provider
//! 调用**：dispatcher 地址、`instance_state`、`port` 在锁内拷贝进
//! [`DispatchTarget`]，三个 guard 全部释放后才执行组件代码。组件 dispatcher 在
//! 调用期间可以自由进入 Core（日志 / task / device...），"持锁调用组件" = 自死锁。
//! panic 收尾（`fail_component`）同样在边界返回之后、无锁状态下执行。
//!
//! # 存活解析（不重复校验 contract / abi）
//!
//! call ABI 不携带 contract / abi：`EndpointId` 是组合期经
//! [`EndpointRegistry::lookup`] / [`EndpointRegistry::discover`] 交付的 opaque
//! capability，contract / abi 已在**交付 id 之前** exact-match 校验。调用只做
//! **存活解析**（[`EndpointRegistry::resolve`]）：死 endpoint / 死 owner 一律
//! 拒绝，绝不把调用重定向到新实例。
//!
//! # 明确不做（下一阶段）
//!
//! - **escape-eligibility scope**：Core 临界区内的 provider panic 保持致命（不是
//!   本次范围）。
//! - **consumer 迁移 / 移除 `kcore_interface_*`**：`component/interface.rs` 语义不变。
//! - **stack pool / 异步调用 / 取消 / drain / 超时**：都不做；service stack 每次
//!   调用现分配（panic 时保守驻留，见 `containment`）。
//!
//! # Phase-1 限制：真实分派只能由 QEMU 证明
//!
//! host fake 上下文后端**不执行组件入口体**：真实 service stack 切换、真实
//! provider panic、以及实际执行中的 A → B → C principal 顺序**不能**由
//! `cargo test` 证明。它们由 QEMU 上用真实导出 `kcomp_service_dispatch` 的组件
//! 验证（后续步骤）；host 用例只覆盖边界记账（re-entry / panic 收尾 / 祖先门禁 /
//! 状态分离），经 test-only 边界辅助函数。

use crate::component::containment::{self, CallOutcome, ServiceDispatch};
use crate::component::endpoint::{EndpointError, EndpointId, EndpointRegistry};
use crate::component::image::ImageTable;
use crate::component::load::ComponentLoadError;
use crate::component::registry::Registry;
use crate::component::{ComponentId, endpoint, image, registry};
use crate::generated::abi::KcompCallFrame;
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
    /// owner 实例记录指向的 image 不在镜像表（Core 不变式破坏，不应发生）→ `ENODEV`。
    ImageMissing,
    /// provider image 没有 `kcomp_service_dispatch`：组件不提供 endpoint 服务
    /// （能力缺失，不是故障）→ `ENOSYS`。
    NoDispatcher,
    /// provider 实例已在当前**同步调用链**上运行（它自己的 task / 外层 service
    /// call / 外层 init 或 exit）：这是重入，不是服务请求 → `EBUSY`。
    Reentrant,
    /// 当前调用链上存在 IRQ 归属作用域（即使藏在嵌套生命周期边界之下）：IRQ
    /// 回调是同步、不可 yield 的顶半部，不得发起通用服务调用 → `EINVAL`
    /// （与 IRQ 上下文中的调度拒绝同档）。
    InIrqContext,
    /// Core 无法分配 per-call service stack（`-ENOMEM`）：provider 入口从未执行，
    /// 传输失败，绝不写 `*out_status`。
    NoServiceStack,
    /// provider dispatcher 在 service 边界内 panic：provider 已被标记 `Failed`
    /// 且其全部 endpoint 永久失效；caller 存活且不变 → `EIO`。
    ProviderFailed,
}

impl From<EndpointError> for CallError {
    fn from(error: EndpointError) -> Self {
        Self::Endpoint(error)
    }
}

/// 锁内拷贝出的分派目标：锁外调用只碰这里的数据（+ 调用方内存）。
struct DispatchTarget {
    dispatcher: ServiceDispatch,
    instance_state: *mut (),
    port: u32,
    provider: ComponentId,
}

/// 锁内准备：存活解析 → re-entry 门禁 → `begin_call` → 取 image dispatcher。
///
/// 调用方必须在一个**作用域**里同时持有 registry / endpoint / image guard 并
/// 在离开作用域后（guard 释放后）才进入 [`containment::call_component_service`]。
fn prepare(
    components: &mut Registry,
    endpoints: &EndpointRegistry,
    images: &ImageTable,
    id: EndpointId,
) -> Result<DispatchTarget, CallError> {
    // (1) 存活解析：死 endpoint / 死 owner 绝不派发（`resolve` 只查存活，
    //     contract / abi 已在组合期交付 id 之前校验）。
    let record = endpoints.resolve(components, id)?;

    // (2) re-entry 门禁：provider 已在当前同步链上（它自己的 task / 外层 service
    //     call / 外层 init 或 exit）→ 重入，不是服务请求。在 `begin_call` 之前
    //     拒绝：不产生需要归还的 inflight。
    if containment::provider_in_active_chain(record.owner) {
        return Err(CallError::Reentrant);
    }

    // (3) owner 的 image / opaque state 在此刻拷贝（`resolve` 刚校验过 owner
    //     存在，故这里是纯读取；拷贝后不再借用 record 之外的记录）。
    let Some(instance) = components.get(record.owner) else {
        return Err(CallError::Endpoint(EndpointError::ProviderNotFound));
    };
    let image_id = instance.image;
    let instance_state = instance.instance_state;

    // (4) inflight 记账门禁：只有 Ready provider 可以开始服务调用；拒绝
    //     （不在 Ready / 溢出 / 未知）统一映射成 EBUSY。此后任何提前返回
    //     都必须归还计数。
    components
        .begin_call(record.owner)
        .map_err(|_| CallError::ProviderBusy)?;

    // (5) image + **可选** dispatcher：缺失 = 组件不提供 endpoint 服务。
    let Some(image) = images.get(image_id) else {
        components.finish_call(record.owner);
        return Err(CallError::ImageMissing);
    };
    let Some(dispatcher) = image.service_dispatch else {
        components.finish_call(record.owner);
        return Err(CallError::NoDispatcher);
    };

    Ok(DispatchTarget {
        // SAFETY: `service_dispatch` 只由 loader 写入（放段后解析 `STT_FUNC`
        // 符号 + 已分配 executable 段边界校验），组件无法伪造；image 常驻
        // （pinned-until-reboot），地址在调用期间有效。
        dispatcher: unsafe { core::mem::transmute::<usize, ServiceDispatch>(dispatcher) },
        instance_state,
        port: record.port,
        provider: record.owner,
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
    dispatch(caller, caller_task, id, method, &frame, out_status)
}

/// 分派核心：`caller` 已由 [`endpoint_call`] 解析（`None` = 无 principal）。
///
/// caller / caller_task 作为显式参数：无 principal / 已 `Failed` 的 `EPERM` 门禁
/// 因此可以脱离进程级边界栈直接测试（`RequestContext` 的 fallback 链由
/// `resource::context` 自己的用例覆盖）。
fn dispatch(
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

    // (2) 祖先上下文门禁：IRQ 回调（即使藏在嵌套生命周期边界之下）不得发起
    //     通用服务调用——它是同步、不可 yield 的顶半部。
    if containment::irq_in_chain() {
        return Err(CallError::InIrqContext);
    }

    // (3) 锁内准备（含 re-entry 门禁）：三个 guard 在本块结束时全部释放——
    //     之后才允许执行组件代码。
    let target = {
        let mut components = registry::get_registry().lock();
        let endpoints = endpoint::get_endpoints().lock();
        let images = image::get_images().lock();
        prepare(&mut components, &endpoints, &images, id)?
    };

    // (4) 无锁派发：走 Core 控制的 service-call 执行边界（per-call service stack
    //     + provider principal + panic containment）。
    let outcome = containment::call_component_service(
        target.provider,
        id,
        caller_task,
        target.dispatcher,
        target.instance_state,
        target.port,
        method,
        frame,
    );

    // (5) 边界返回后的收尾（panic / 无栈 / 正常三条分类）。
    complete_call(target.provider, outcome, out_status)
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
    use crate::component::containment;
    use crate::component::endpoint::{ContractId, EndpointError};
    use crate::component::interface::{InterfaceAbi, InterfaceKind};
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

    /// 初始化全局真相，在全局 image 表登记一份测试 image，声明一个 Ready 实例
    /// （记录 `instance_state`），返回 provider 实例 id。
    fn ready_provider(
        name: &[u8],
        dispatcher: Option<usize>,
        instance_state: *mut (),
    ) -> ComponentId {
        registry::init();
        endpoint::init();
        image::init();
        let image = image::test_support::register_test_image_with_dispatch(name, 0, dispatcher);
        let mut reg = registry::get_registry().lock();
        let id = reg.declare(image).unwrap();
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
            .invalidate_endpoint(endpoint);
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
                .declare(crate::component::image::ComponentImageId::from_raw(0xCA11))
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

    /// 生成物里的 `const _` 断言在编译期覆盖；这里在 host 上再显式钉一次，
    /// 并钉住扁平字段顺序（args, args_len, input, input_len, output, output_len）。
    #[test]
    fn call_frame_is_six_pointer_widths() {
        let pointer = core::mem::size_of::<usize>();
        assert_eq!(core::mem::size_of::<KcompCallFrame>(), 6 * pointer);
        assert_eq!(
            core::mem::align_of::<KcompCallFrame>(),
            core::mem::align_of::<usize>()
        );
        assert_eq!(core::mem::offset_of!(KcompCallFrame, args), 0);
        assert_eq!(core::mem::offset_of!(KcompCallFrame, args_len), pointer);
        assert_eq!(core::mem::offset_of!(KcompCallFrame, input), 2 * pointer);
        assert_eq!(
            core::mem::offset_of!(KcompCallFrame, input_len),
            3 * pointer
        );
        assert_eq!(core::mem::offset_of!(KcompCallFrame, output), 4 * pointer);
        assert_eq!(
            core::mem::offset_of!(KcompCallFrame, output_len),
            5 * pointer
        );
    }

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
            containment::with_test_init_boundary(Some(ComponentId::from_raw(5)), || {
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
        crate::component::interface::init();
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
        crate::component::interface::init();
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
}
