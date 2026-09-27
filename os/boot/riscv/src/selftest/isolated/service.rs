//! KernelNative caller → Isolated provider cross-AS service dispatch: the caller
//! frame is delivered directly (shared Core mappings), and provider faults are
//! contained.

// -----------------------------------------------------------------------
// KernelNative caller → Isolated provider 的跨域 service Gate。
//
// provider `kcomp_isolated_svc` 经生产路径创建（私有 AS + 按域镜像 + Core
// 预置窗口）；caller 是真实 KernelNative 组件（`kcomp_smoke`）的任务边界。
// Core 从自己的视图读回 provider 的上报区（窗口 backing），断言：
//   - 扁平帧**没有中间页**：provider 看到的 frame / args / input / output 指针
//     就是 caller 的地址，三个缓冲在 provider 的 AS 里 same VA → same PA
//     直接可读（无拷贝）；
//   - provider 在私有 AS 里运行（satp / tp），Core AS 每次切换后恢复；
//   - provider 故障（trap）被 Core 收敛：caller 拿到类型化错误、实例
//     Failed + AS 退役 + 窗口归还、Core 存活。
// -----------------------------------------------------------------------

/// `kcomp_isolated_svc` 的上报槽号（与组件源码逐槽一致）。
pub(crate) const SVC_REPORT_OFF: usize = 512;
pub(crate) const SVC_R_MAGIC: usize = 0;
pub(crate) const SVC_R_STATE: usize = 1;
pub(crate) const SVC_R_PORT: usize = 2;
pub(crate) const SVC_R_METHOD: usize = 3;
pub(crate) const SVC_R_FRAME: usize = 4;
pub(crate) const SVC_R_ARGS: usize = 5;
pub(crate) const SVC_R_ARGS_LEN: usize = 6;
pub(crate) const SVC_R_INPUT: usize = 7;
pub(crate) const SVC_R_INPUT_LEN: usize = 8;
pub(crate) const SVC_R_OUTPUT: usize = 9;
pub(crate) const SVC_R_OUTPUT_LEN: usize = 10;
pub(crate) const SVC_R_ARG0: usize = 11;
pub(crate) const SVC_R_ARG1: usize = 12;
pub(crate) const SVC_R_IN0: usize = 13;
pub(crate) const SVC_R_IN1: usize = 14;
pub(crate) const SVC_R_TP: usize = 15;
pub(crate) const SVC_R_SATP: usize = 16;
pub(crate) const SVC_R_CALLS: usize = 17;
pub(crate) const SVC_R_CREATE_MAGIC: usize = 18;
pub(crate) const SVC_R_DESTROY_MAGIC: usize = 19;
pub(crate) const SVC_R_FAULT_TARGET: usize = 20;

pub(crate) const SVC_REPORT_MAGIC: usize = 0x5356_4321; // "SVC!"
pub(crate) const SVC_CREATE_MAGIC: usize = 0x4352_4541; // "CREA"
pub(crate) const SVC_DESTROY_MAGIC: usize = 0x4445_5354; // "DEST"
/// Core 侧登记 endpoint 时使用的 port（与组件源码一致）。
pub(crate) const SVC_PORT: u32 = 0x1001;
pub(crate) const SVC_METHOD_ECHO: u32 = 0x2001;
pub(crate) const SVC_METHOD_FAULT: u32 = 0x2002;
pub(crate) const SVC_STATUS_OK: i32 = 0x5E;
pub(crate) const SVC_ECHO_XOR: u8 = 0x5A;
/// 本用例的 contract / abi（组合方提供；Core 只比较）。
pub(crate) const SVC_CONTRACT: u64 = 0x4953_4F4C_5356_4301;
pub(crate) const SVC_ABI: u64 = 0x4953_4F4C_5356_4302;

pub(crate) static SVC_FAULT_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SVC_FAULT_CAUSE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SVC_FAULT_STVAL: AtomicUsize = AtomicUsize::new(0);

/// 一个已创建、已登记 endpoint 的 Isolated service provider。
pub(crate) struct SvcProvider {
    pub(crate) id: ComponentId,
    pub(crate) handle: AddressSpaceHandle,
    pub(crate) window_pa: usize,
    pub(crate) endpoint: kernel::component::endpoint::EndpointId,
}

/// 从实例窗口 backing 的 Core 视图读一个上报槽。
///
/// # Safety
/// `window_pa` 必须是本用例实例窗口 backing 的基址（仍驻留）。
pub(crate) unsafe fn svc_slot(window_pa: usize, index: usize) -> usize {
    unsafe {
        let base = (window_pa + SVC_REPORT_OFF) as *const usize;
        core::ptr::read_volatile(base.add(index))
    }
}

/// 经生产路径创建 `kcomp_isolated_svc`，并从 Core 侧登记它的 endpoint。
///
/// Isolated provider **不能自己 publish**（没有组件→Core 的 publish
/// trampoline）：组合方（本用例，白盒）在 provider Ready 之后 stage + commit +
/// discover。endpoint 真相仍由 Core 拥有；`port` 由 Core 原样透传给 dispatcher。
pub(crate) fn svc_provider() -> SvcProvider {
    use kernel::component::abi::{InterfaceAbi, InterfaceKind};
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::{self, ContractId, ExecutionDomain};
    use kernel::component::isolated_lifecycle;
    use kernel::component::load;
    use kernel::component::registry;

    let id = match load::create_component(
        b"kcomp_isolated_svc",
        &KcompCreateArgs::empty(),
        ExecutionDomain::IsolatedNative,
    ) {
        Ok(id) => id,
        Err(error) => {
            kernel::log!("selftest", "isolated-service: create failed: {:?}", error);
            fail("isolated-service: provider create failed");
        }
    };
    let handle = {
        let reg = registry::get_registry().lock();
        match reg.get(id).and_then(|record| record.address_space) {
            Some(handle) => handle,
            None => fail("isolated-service: provider has no address space"),
        }
    };
    let window_pa = match address_space::mapping_exact(handle, &isolated_lifecycle::window_range())
    {
        Ok(Some(mapping)) => mapping.physical_range.base,
        _ => fail("isolated-service: instance window is not mapped"),
    };
    // create 真的在私有 AS 里跑过（上报槽）。
    if unsafe { svc_slot(window_pa, SVC_R_CREATE_MAGIC) } != SVC_CREATE_MAGIC {
        fail("isolated-service: provider create did not run in its private AS");
    }
    let endpoint = {
        let reg = registry::get_registry().lock();
        let mut endpoints = endpoint::get_endpoints().lock();
        if endpoints
            .stage_publish(
                &reg,
                id,
                b"svc.isolated",
                ContractId::from_raw(SVC_CONTRACT),
                InterfaceKind::Device,
                InterfaceAbi::from_raw(SVC_ABI),
                SVC_PORT,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .is_err()
        {
            fail("isolated-service: stage_publish failed");
        }
        if endpoints.commit_pending(&reg, id).is_err() {
            fail("isolated-service: commit_pending failed");
        }
        match endpoints.discover(
            &reg,
            id,
            b"svc.isolated",
            ContractId::from_raw(SVC_CONTRACT),
        ) {
            Ok(endpoint) => endpoint,
            Err(_) => fail("isolated-service: discover failed"),
        }
    };
    SvcProvider {
        id,
        handle,
        window_pa,
        endpoint,
    }
}

/// 在 KernelNative caller 的任务边界里执行 `f`（ArchTest 白盒：直接装任务
/// 边界，与调度器同一条 `containment::enter_task` 路径）。
pub(crate) fn with_kernel_caller<R>(caller: ComponentId, task: u32, f: impl FnOnce() -> R) -> R {
    use kernel::component::containment;
    containment::enter_task(kernel::task::TaskId::from_raw(task), caller);
    let result = f();
    containment::enter_anchor();
    result
}

/// 主用例：KernelNative caller → Isolated provider 端到端，帧被拷贝、结果正确、
/// provider 在私有 AS 里运行、caller 内存不可达、Core AS 恢复。
pub(crate) fn isolated_service() -> ! {
    use kernel::component::abi::InterfaceAbi;
    use kernel::component::call;
    use kernel::component::endpoint::{self, ContractId, ExecutionDomain, Mechanism};
    use kernel::component::isolated_lifecycle;
    use kernel::component::load;
    use kernel::component::registry;

    let core_satp = read_satp();
    let provider = svc_provider();
    let caller = match load::load_and_start(b"kcomp_smoke", ExecutionDomain::KernelNative) {
        Ok(id) => id,
        Err(_) => fail("isolated-service: caller load failed"),
    };

    // bind：KernelNative caller → Isolated provider 必须选 Gate（binding 只携带
    // opaque EndpointId + port；绝不交付 provider 域内的裸入口）。
    let bound = {
        let reg = registry::get_registry().lock();
        endpoint::get_endpoints().lock().bind(
            &reg,
            provider.endpoint,
            ContractId::from_raw(SVC_CONTRACT),
            InterfaceAbi::from_raw(SVC_ABI),
            ExecutionDomain::KernelNative,
        )
    };
    match bound {
        Ok(bound) if bound.mechanism == Mechanism::Gate => {}
        _ => fail("isolated-service: bind did not select Gate"),
    }

    // When：caller 经 Core call gate 调用 provider（method = ECHO）。
    let args = [0xA1u8, 0xB2];
    let input = [0x11u8, 0x22, 0x33];
    let mut output = [0u8; 4];
    let mut out_status = 0i32;
    let transport = with_kernel_caller(caller, 0x6A, || {
        call::endpoint_call(
            provider.endpoint,
            SVC_METHOD_ECHO,
            args.as_ptr(),
            args.len(),
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
            &mut out_status,
        )
    });

    // Then 1：传输成功、provider 返回值 = 方法状态、output 被拷回 caller 缓冲。
    if transport != Ok(()) {
        kernel::log!(
            "selftest",
            "isolated-service: transport error: {:?}",
            transport
        );
        fail("isolated-service: transport failed");
    }
    if out_status != SVC_STATUS_OK {
        fail("isolated-service: provider status mismatch");
    }
    let expected = [
        input[0] ^ SVC_ECHO_XOR,
        input[1] ^ SVC_ECHO_XOR,
        input[2] ^ SVC_ECHO_XOR,
        input[0] ^ SVC_ECHO_XOR,
    ];
    if output != expected {
        fail("isolated-service: output was not copied back to the caller buffer");
    }
    if read_satp() != core_satp {
        fail("isolated-service: Core satp not restored after the transition");
    }

    // Then 2：provider 的上报（从 Core 视图读回）证明帧**直接**交付、没有拷贝。
    let window = isolated_lifecycle::window_range();
    let slot = |index: usize| unsafe { svc_slot(provider.window_pa, index) };
    if slot(SVC_R_MAGIC) != SVC_REPORT_MAGIC || slot(SVC_R_CALLS) != 1 {
        fail("isolated-service: provider dispatcher did not run exactly once");
    }
    if slot(SVC_R_STATE) != window.base + SVC_REPORT_OFF {
        fail("isolated-service: provider did not receive its opaque state");
    }
    if slot(SVC_R_PORT) != SVC_PORT as usize || slot(SVC_R_METHOD) != SVC_METHOD_ECHO as usize {
        fail("isolated-service: port / method were not delivered");
    }
    // 三个负载指针就是 caller 的地址（无拷贝）；帧描述符不是实例私有页。
    if slot(SVC_R_ARGS) != args.as_ptr() as usize
        || slot(SVC_R_INPUT) != input.as_ptr() as usize
        || slot(SVC_R_OUTPUT) != output.as_mut_ptr() as usize
    {
        fail("isolated-service: provider pointers are not the caller's buffers");
    }
    let frame_ptr = slot(SVC_R_FRAME);
    let instance_stack_end =
        isolated_lifecycle::ISOLATED_STACK_BASE + isolated_lifecycle::ISOLATED_STACK_SIZE;
    if frame_ptr == 0
        || (frame_ptr >= window.base && frame_ptr < window.base + window.size)
        || (frame_ptr >= isolated_lifecycle::ISOLATED_STACK_BASE && frame_ptr < instance_stack_end)
    {
        fail("isolated-service: provider frame is not a shared Core pointer");
    }
    if slot(SVC_R_ARGS_LEN) != args.len()
        || slot(SVC_R_INPUT_LEN) != input.len()
        || slot(SVC_R_OUTPUT_LEN) != output.len()
    {
        fail("isolated-service: frame lengths were not preserved");
    }
    if slot(SVC_R_ARG0) != args[0] as usize
        || slot(SVC_R_ARG1) != args[1] as usize
        || slot(SVC_R_IN0) != input[0] as usize
        || slot(SVC_R_IN1) != input[1] as usize
    {
        fail("isolated-service: provider could not read the caller payload in place");
    }
    // provider 在私有 AS 里运行：satp = 实例 root、tp = Core 安装的 runtime slot。
    let expected_satp = match address_space::prepare_activation(provider.handle) {
        Ok(activation) => activation.token().satp(),
        Err(_) => fail("isolated-service: prepare_activation failed"),
    };
    if slot(SVC_R_SATP) != expected_satp || expected_satp == core_satp {
        fail("isolated-service: provider did not run on its private root");
    }
    if slot(SVC_R_TP) != window.base + isolated_lifecycle::WINDOW_RUNTIME_OFF {
        fail("isolated-service: provider runtime slot (tp) mismatch");
    }

    // Then 3：**共享 Core 映射**让 caller 的缓冲在 provider 的 AS 里直接有效
    // （same VA → same PA）——这正是 KernelNative caller 能直接交付帧的原因；
    // 同 PA 由原地读写证明（payload 逐字节相等 + output 原地回显），这里只钉
    // "provider 的页表真的翻译这些 caller VA"。真正不可达的是别的实例的私有
    // 映射（见 `isolated-private-unreachable`）。
    for va in [
        args.as_ptr() as usize,
        input.as_ptr() as usize,
        output.as_mut_ptr() as usize,
    ] {
        if !matches!(address_space::translate(provider.handle, va), Ok(Some(_))) {
            fail("isolated-service: caller buffer is not mapped in the provider AS");
        }
    }

    // Then 4：inflight 归还、实例仍 Ready、endpoint 仍 Live。
    if registry::get_registry().lock().active_calls(provider.id) != 0 {
        fail("isolated-service: inflight was not returned");
    }
    if registry::get_registry()
        .lock()
        .get(provider.id)
        .map(|record| record.state)
        != Some(kernel::component::ComponentState::Ready)
    {
        fail("isolated-service: provider left Ready after a successful call");
    }
    {
        let reg = registry::get_registry().lock();
        let live = endpoint::get_endpoints()
            .lock()
            .lookup(
                &reg,
                provider.endpoint,
                ContractId::from_raw(SVC_CONTRACT),
                InterfaceAbi::from_raw(SVC_ABI),
            )
            .is_ok();
        if !live {
            fail("isolated-service: endpoint died after a successful call");
        }
    }

    // Then 5：生命周期仍然可用（stop 走私有 AS 里的 destroy）。
    if kernel::component::stop_component(provider.id).is_err() {
        fail("isolated-service: stop failed");
    }
    if unsafe { svc_slot(provider.window_pa, SVC_R_DESTROY_MAGIC) } != SVC_DESTROY_MAGIC {
        fail("isolated-service: destroy entry did not run");
    }
    kernel::log!(
        "selftest",
        "isolated-service: gate OK: id={}, frame={:#x}, satp={:#x}",
        provider.id.raw(),
        frame_ptr,
        expected_satp
    );
    pass("isolated-service")
}

/// 故障用例：provider 在 dispatch 期间访问**未映射地址** → 私有 AS 缺页
/// → 普通 trap 路径的异常钩子（本用例策略观察后 Abandon）→ caller 拿到类型化
/// 错误、实例 Failed + AS 退役 + 窗口归还、Core 存活。
pub(crate) fn isolated_service_fault() -> ! {
    use kernel::component::call::{self, CallError};
    use kernel::component::endpoint::{EndpointError, ExecutionDomain};
    use kernel::component::isolated_lifecycle;
    use kernel::component::load;
    use kernel::component::registry;
    use kernel::component::runtime_slot;
    use kernel::component::ComponentState;
    use kernel::errno::Errno;

    let core_satp = read_satp();
    let provider = svc_provider();
    let caller = match load::load_and_start(b"kcomp_smoke", ExecutionDomain::KernelNative) {
        Ok(id) => id,
        Err(_) => fail("isolated-service-fault: caller load failed"),
    };

    // 目标 = 未映射 VA：在任何 AS 里都缺页（caller 的 Core 栈在共享模型下对
    // provider 可见，不能再拿它当"不可达"探针）。
    let target_va = ISOLATED_ABANDON_VA;
    let target_bytes = target_va.to_le_bytes();

    SVC_FAULT_COUNT.store(0, Ordering::Release);
    SVC_FAULT_CAUSE.store(0, Ordering::Release);
    SVC_FAULT_STVAL.store(0, Ordering::Release);
    FAULT_HANDLER_SATP.store(0, Ordering::Release);
    FAULT_HANDLER_SP.store(0, Ordering::Release);
    isolated::install();
    if !isolated::register_fault_policy(svc_fault_policy) {
        fail("isolated-service-fault: fault policy registration failed");
    }

    let mut out_status = 0i32;
    let transport = with_kernel_caller(caller, 0x6C, || {
        call::endpoint_call(
            provider.endpoint,
            SVC_METHOD_FAULT,
            target_bytes.as_ptr(),
            target_bytes.len(),
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
    });

    // Then 1：caller 拿到类型化错误（EIO），不写 out_status；Core AS 恢复。
    match transport {
        Err(CallError::ProviderFailed) => {}
        other => {
            kernel::log!(
                "selftest",
                "isolated-service-fault: unexpected transport: {:?}",
                other
            );
            fail("isolated-service-fault: expected ProviderFailed");
        }
    }
    if Errno::from(CallError::ProviderFailed) != Errno::EIO {
        fail("isolated-service-fault: errno mismatch");
    }
    if out_status != 0 {
        fail("isolated-service-fault: failed call wrote out_status");
    }
    if read_satp() != core_satp {
        fail("isolated-service-fault: Core satp not restored after the fault");
    }

    // Then 2：故障确实发生在 provider 上下文，且被 Core 在 Core root / Core
    // 专用 trap 栈上处理；故障地址 = caller 的缓冲（实例 AS 不可达）。
    if SVC_FAULT_COUNT.load(Ordering::Acquire) != 1 {
        fail("isolated-service-fault: fault policy did not run exactly once");
    }
    if SVC_FAULT_CAUSE.load(Ordering::Acquire) != 13 {
        fail("isolated-service-fault: expected a load page fault (scause 0xd)");
    }
    if SVC_FAULT_STVAL.load(Ordering::Acquire) != target_va {
        fail("isolated-service-fault: stval is not the unmapped target");
    }
    // provider 真实运行在私有 AS 里：trap 处理现场 satp == 实例 root（普通 trap
    // 路径不切回 Core root），且 != Core root。
    let provider_satp = FAULT_HANDLER_SATP.load(Ordering::Acquire);
    if provider_satp == 0 || provider_satp == core_satp {
        fail("isolated-service-fault: provider did not run on a private root");
    }
    assert_fault_ran_in_instance_context("isolated-service-fault", provider_satp);
    // provider 的上报证明它读到了 caller 帧里的目标地址（在 fault 之前）。
    if unsafe { svc_slot(provider.window_pa, SVC_R_FAULT_TARGET) } != target_va {
        fail("isolated-service-fault: provider did not see the fault target");
    }

    // Then 3：实例逻辑死亡 + AS 退役 + Core 预置窗口（栈 / 窗口）归还 +
    // runtime slot 清空 + endpoint 永久失效 + inflight 归还。
    if registry::get_registry()
        .lock()
        .get(provider.id)
        .map(|record| record.state)
        != Some(ComponentState::Failed)
    {
        fail("isolated-service-fault: provider was not marked Failed");
    }
    if registry::get_registry().lock().active_calls(provider.id) != 0 {
        fail("isolated-service-fault: inflight was not returned");
    }
    if !runtime_slot::get_slots().lock().get(provider.id).is_null() {
        fail("isolated-service-fault: runtime slot was not cleared");
    }
    match address_space::prepare_activation(provider.handle) {
        Err(kernel::memory::address_space::MapError::Retired) => {}
        _ => fail("isolated-service-fault: address space was not retired"),
    }
    for range in [
        isolated_lifecycle::stack_range(),
        isolated_lifecycle::window_range(),
    ] {
        if !matches!(
            address_space::mapping_exact(provider.handle, &range),
            Ok(None)
        ) {
            fail("isolated-service-fault: a Core-prepared window leaked");
        }
    }
    {
        let reg = registry::get_registry().lock();
        let dead = kernel::component::endpoint::get_endpoints()
            .lock()
            .resolve(&reg, provider.endpoint)
            .is_err();
        if !dead {
            fail("isolated-service-fault: provider endpoint was not invalidated");
        }
    }
    // stale endpoint：再次调用在 Core 边界被拒（`resolve` 先于任何进入；
    // 实例已 Failed、AS 已退役、窗口已归还 → 不可能再执行 provider）。
    let transport = with_kernel_caller(caller, 0x71, || {
        call::endpoint_call(
            provider.endpoint,
            SVC_METHOD_ECHO,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut out_status,
        )
    });
    match transport {
        Err(CallError::Endpoint(EndpointError::EndpointDead)) => {}
        other => {
            kernel::log!(
                "selftest",
                "isolated-service-fault: stale call was not blocked: {:?}",
                other
            );
            fail("isolated-service-fault: stale endpoint was not blocked");
        }
    }
    if registry::get_registry().lock().active_calls(provider.id) != 0 {
        fail("isolated-service-fault: stale call leaked inflight");
    }
    if read_satp() != core_satp {
        fail("isolated-service-fault: Core satp not restored after the stale call");
    }
    if !kernel_native_still_works() {
        fail("isolated-service-fault: KernelNative path broke after the fault");
    }
    kernel::log!(
        "selftest",
        "isolated-service-fault: contained: id={}, scause={:#x}, stval={:#x}",
        provider.id.raw(),
        SVC_FAULT_CAUSE.load(Ordering::Acquire),
        SVC_FAULT_STVAL.load(Ordering::Acquire)
    );
    pass("isolated-service-fault")
}

use super::*;
