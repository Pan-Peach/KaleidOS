//! Endpoint call —— `kcore_endpoint_call` 的 Core 实现（**call ABI 管道**）。
//!
//! # 定位
//!
//! 组合期用 `kcore_endpoint_lookup` 把 `(provider, port_name, contract)` 解析成
//! opaque [`EndpointId`]；本模块把它变成**一次真实调用**：
//!
//! ```text
//! caller（最内层活动执行边界，RequestContext::ambient）
//!   → resolve endpoint（存活：endpoint Live + owner 存在且 Ready）
//!   → registry.begin_call(provider)（Ready 门禁 + inflight 记账）
//!   → 取 provider image 的可选 kcomp_service_dispatch + instance_state + port
//!   → 【无锁】dispatcher(instance_state, port, method, &frame)
//!   → registry.finish_call(provider)
//! ```
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
//!
//! # 存活解析（不重复校验 contract / abi）
//!
//! call ABI 不携带 contract / abi：`EndpointId` 是组合期经
//! [`EndpointRegistry::lookup`] / [`EndpointRegistry::discover`] 交付的 opaque
//! capability，contract / abi 已在**交付 id 之前** exact-match 校验。调用只做
//! **存活解析**（[`EndpointRegistry::resolve`]）：死 endpoint / 死 owner 一律
//! 拒绝，绝不把调用重定向到新实例。
//!
//! # 本阶段限制（Phase B：KernelNative 直接分派）
//!
//! 本模块是**直接函数调用**，没有执行边界 / service stack（那是下一阶段）：
//!
//! - **无执行边界 / 无 principal 切换**：调用期间 `RequestContext::ambient()`
//!   仍是 **caller** 的身份——provider 在自己 dispatcher 里发起的 Core 调用
//!   会被记到 caller 名下。KernelNative 是受信代码，这是刻意的信任模型
//!   （`AGENTS.md`：部署形态本身就是安全策略），不是安全边界。
//! - **无 re-entry 检测**：provider 递归调用另一个 endpoint（包括自己）不被拒绝。
//! - **无 provider panic containment**：dispatcher 内 panic 时 `panic_escape`
//!   找不到属于 provider 的边界（caller 的边界仍活动），按现有语义会逃逸到
//!   **caller** 的边界并杀死 caller 的实例；provider 的 inflight 计数也不会归还
//!   （`finish_call` 被跳过）。
//!
//! 以上都由下一阶段的执行边界 / service stack 统一解决；本阶段只把**管道**
//! （flat frame、dispatcher 解析、inflight 记账、传输 / 方法状态分离）落地。

use crate::component::endpoint::{EndpointError, EndpointId, EndpointRegistry};
use crate::component::image::ImageTable;
use crate::component::registry::Registry;
use crate::component::{ComponentId, endpoint, image, registry};
use crate::generated::abi::KcompCallFrame;
use crate::resource::RequestContext;

/// `kcomp_service_dispatch` 的 Core 侧函数类型（手写镜像 `abi/component.toml` 的
/// `KcompServiceDispatch`，与 `containment.rs` 的 `InstanceCreate` /
/// `InstanceDestroy` 同款）。
type ServiceDispatch = extern "C" fn(*mut (), u32, u32, *const KcompCallFrame) -> i32;

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

/// 锁内准备：存活解析 → `begin_call` → 取 image dispatcher。
///
/// 调用方必须在一个**作用域**里同时持有 registry / endpoint / image guard 并
/// 在离开作用域后（guard 释放后）才执行 [`invoke`]。
fn prepare(
    components: &mut Registry,
    endpoints: &EndpointRegistry,
    images: &ImageTable,
    id: EndpointId,
) -> Result<DispatchTarget, CallError> {
    // (1) 存活解析：死 endpoint / 死 owner 绝不派发（`resolve` 只查存活，
    //     contract / abi 已在组合期交付 id 之前校验）。
    let record = endpoints.resolve(components, id)?;

    // (2) owner 的 image / opaque state 在此刻拷贝（`resolve` 刚校验过 owner
    //     存在，故这里是纯读取；拷贝后不再借用 record 之外的记录）。
    let Some(instance) = components.get(record.owner) else {
        return Err(CallError::Endpoint(EndpointError::ProviderNotFound));
    };
    let image_id = instance.image;
    let instance_state = instance.instance_state;

    // (3) inflight 记账门禁：只有 Ready provider 可以开始服务调用；拒绝
    //     （不在 Ready / 溢出 / 未知）统一映射成 EBUSY。此后任何提前返回
    //     都必须归还计数。
    components
        .begin_call(record.owner)
        .map_err(|_| CallError::ProviderBusy)?;

    // (4) image + **可选** dispatcher：缺失 = 组件不提供 endpoint 服务。
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

/// 锁外调用：dispatcher / `instance_state` / `port` 全部来自锁内拷贝。
fn invoke(target: &DispatchTarget, method: u32, frame: &KcompCallFrame) -> i32 {
    (target.dispatcher)(
        target.instance_state,
        target.port,
        method,
        frame as *const KcompCallFrame,
    )
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
    let caller = RequestContext::ambient().map(|ctx| ctx.component);
    dispatch(caller, id, method, &frame, out_status)
}

/// 分派核心：`caller` 已由 [`endpoint_call`] 解析（`None` = 无 principal）。
///
/// caller 作为显式参数：无 principal / 已 `Failed` 的 `EPERM` 门禁因此可以脱离
/// 进程级边界栈直接测试（`RequestContext` 的 fallback 链由 `resource::context`
/// 自己的用例覆盖）。
fn dispatch(
    caller: Option<ComponentId>,
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

    // (2) 锁内准备：三个 guard 在本块结束时全部释放——之后才允许执行组件代码。
    let target = {
        let mut components = registry::get_registry().lock();
        let endpoints = endpoint::get_endpoints().lock();
        let images = image::get_images().lock();
        prepare(&mut components, &endpoints, &images, id)?
    };

    // (3) 无锁派发。
    let provider_status = invoke(&target, method, frame);

    // (4) 归还 inflight（`begin_call` 一定成功过；无条件归还，不设门禁）。
    //     provider panic 会跳过这里——containment 属下一阶段，见模块文档。
    registry::get_registry().lock().finish_call(target.provider);

    // (5) 传输成功：provider status 写入 out（仅在此时有意义；与传输状态分离）。
    // SAFETY: `out_status` 由调用方保证可写（C ABI 契约；入口已校验非空）；
    // unaligned 写防未对齐 UB。
    unsafe { core::ptr::write_unaligned(out_status, provider_status) };
    Ok(())
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

    // -- 1. frame 读写 + provider status 落位 ---------------------------------

    #[test]
    fn dispatch_reads_and_writes_frame_and_keeps_provider_status_separate() {
        // Given：一个带 echo dispatcher 的 Ready provider 与一个 Live endpoint。
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

        // Then 1：传输 0；provider 的返回值落在 *out_status（两个状态不混）。
        assert_eq!(transport, Ok(()));
        assert_eq!(out_status, 0x2A);
        // Then 2：dispatcher 收到正确的 port / method，能读 args / input、写 output。
        assert_eq!(seen.calls, 1);
        assert_eq!(seen.port, PORT, "provider 定义的 dispatch token 原样传递");
        assert_eq!(seen.method, 42, "method 由 Core 原样传递");
        assert_eq!(seen.args, args);
        assert_eq!(seen.input, input);
        assert_eq!(output, [0xA5, 0x5A, 0xC3]);
        // Then 3：inflight 记账已归还。
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);

        containment::enter_anchor();
    }

    // -- 2. 业务 errno 不冒充传输失败 -----------------------------------------

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
        let endpoint = publish(provider, b"svc.errno");
        enter_caller(12);

        let mut out_status = 0i32;
        // When：provider 返回 -EIO（空 payload：合法的退化 frame）。
        let transport = endpoint_call(
            endpoint,
            1,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        );

        // Then：传输仍是 0；负值只在 *out_status，绝不与 Core 失败混淆。
        assert_eq!(transport, Ok(()));
        assert_eq!(out_status, Errno::EIO.code());
        assert_ne!(out_status, 0, "provider 的 errno 不是传输状态的 0");
        assert_eq!(registry::get_registry().lock().active_calls(provider), 0);

        containment::enter_anchor();
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
        let error = dispatch(None, endpoint, 0, &EMPTY_FRAME, &mut out_status).unwrap_err();
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
        let error = dispatch(Some(failed), endpoint, 0, &EMPTY_FRAME, &mut out_status).unwrap_err();
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
}
