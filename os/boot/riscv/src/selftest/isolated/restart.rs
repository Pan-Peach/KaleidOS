//! Fault containment on an already-serving instance and logical restart of the
//! same image as a genuinely independent fresh instance.

/// （新 AS / 新窗口 / 新 slot / 新 endpoint）并再次服务。
pub(crate) fn isolated_ready_fault() -> ! {
    use kernel::component::abi::InterfaceAbi;
    use kernel::component::call::{self, CallError};
    use kernel::component::endpoint::{
        self, ContractId, EndpointError, ExecutionDomain, Mechanism,
    };
    use kernel::component::isolated_lifecycle;
    use kernel::component::load;
    use kernel::component::registry;
    use kernel::component::runtime_slot;
    use kernel::component::ComponentState;

    let core_satp = read_satp();
    let first = svc_provider();
    let caller = match load::load_and_start(b"kcomp_smoke", ExecutionDomain::KernelNative) {
        Ok(id) => id,
        Err(_) => fail_case("isolated-ready-fault", "caller load failed"),
    };
    let bound = {
        let reg = registry::get_registry().lock();
        endpoint::get_endpoints().lock().bind(
            &reg,
            first.endpoint,
            ContractId::from_raw(SVC_CONTRACT),
            InterfaceAbi::from_raw(SVC_ABI),
            ExecutionDomain::KernelNative,
        )
    };
    match bound {
        Ok(bound) if bound.mechanism == Mechanism::Gate => {}
        _ => fail_case("isolated-ready-fault", "bind did not select Gate"),
    }

    // (1) 先健康服务一次：Ready、call counter = 1（故障发生在"已经跑起来"的
    //     实例上，不是首次调用的失败）。
    let input = [0x33u8, 0x44];
    let mut output = [0u8; 2];
    let mut out_status = 0i32;
    let transport = with_kernel_caller(caller, 0x6F, || {
        call::endpoint_call(
            first.endpoint,
            SVC_METHOD_ECHO,
            core::ptr::null(),
            0,
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
            &mut out_status,
        )
    });
    if transport != Ok(()) || out_status != SVC_STATUS_OK {
        fail_case("isolated-ready-fault", "healthy call failed");
    }
    if unsafe { svc_slot(first.window_pa, SVC_R_CALLS) } != 1 {
        fail_case("isolated-ready-fault", "healthy call was not observed");
    }

    // (2) 故障：provider 读未映射地址（任何 AS 都缺页）。
    SVC_FAULT_COUNT.store(0, Ordering::Release);
    SVC_FAULT_CAUSE.store(0, Ordering::Release);
    SVC_FAULT_STVAL.store(0, Ordering::Release);
    isolated::install();
    if !isolated::register_fault_policy(svc_fault_policy) {
        fail_case("isolated-ready-fault", "fault policy registration failed");
    }
    let target_va = ISOLATED_ABANDON_VA;
    let target_bytes = target_va.to_le_bytes();
    let transport = with_kernel_caller(caller, 0x70, || {
        call::endpoint_call(
            first.endpoint,
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
    match transport {
        Err(CallError::ProviderFailed) => {}
        other => {
            kernel::log!(
                "selftest",
                "isolated-ready-fault: unexpected transport: {:?}",
                other
            );
            fail_case("isolated-ready-fault", "expected ProviderFailed");
        }
    }
    if read_satp() != core_satp {
        fail_case(
            "isolated-ready-fault",
            "Core satp not restored after the fault",
        );
    }
    assert_failure_released("isolated-ready-fault", first.id, first.handle);
    {
        let reg = registry::get_registry().lock();
        let dead = endpoint::get_endpoints()
            .lock()
            .resolve(&reg, first.endpoint)
            .is_err();
        if !dead {
            fail_case(
                "isolated-ready-fault",
                "provider endpoint was not invalidated",
            );
        }
    }

    // (3) stale endpoint：第二次调用在 Core 边界被拒（provider 从未再次进入）。
    let transport = with_kernel_caller(caller, 0x72, || {
        call::endpoint_call(
            first.endpoint,
            SVC_METHOD_ECHO,
            core::ptr::null(),
            0,
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
            &mut out_status,
        )
    });
    match transport {
        Err(CallError::Endpoint(EndpointError::EndpointDead)) => {}
        other => {
            kernel::log!(
                "selftest",
                "isolated-ready-fault: stale call was not blocked: {:?}",
                other
            );
            fail_case("isolated-ready-fault", "stale endpoint was not blocked");
        }
    }
    if registry::get_registry().lock().active_calls(first.id) != 0 {
        fail_case("isolated-ready-fault", "stale call leaked inflight");
    }

    // (4) 逻辑重启：同一 image 的全新实例（fresh AS / window / slot）。
    let second = svc_provider();
    if second.id == first.id {
        fail_case("isolated-ready-fault", "restart reused the ComponentId");
    }
    if second.handle.raw_id() == first.handle.raw_id() {
        fail_case("isolated-ready-fault", "restart reused the address space");
    }
    if second.window_pa == 0 {
        fail_case("isolated-ready-fault", "fresh instance has no window");
    }
    if registry_state(first.id) != Some(ComponentState::Failed) {
        fail_case("isolated-ready-fault", "old instance tombstone was lost");
    }
    let expected_slot =
        isolated_lifecycle::window_range().base + isolated_lifecycle::WINDOW_RUNTIME_OFF;
    if runtime_slot::get_slots().lock().get(second.id) as usize != expected_slot {
        fail_case("isolated-ready-fault", "fresh instance has no runtime slot");
    }
    if !runtime_slot::get_slots().lock().get(first.id).is_null() {
        fail_case(
            "isolated-ready-fault",
            "dead instance kept its runtime slot",
        );
    }
    if second.endpoint == first.endpoint {
        fail_case("isolated-ready-fault", "restart reused the endpoint");
    }

    // (5) 新实例独立且可用：ECHO 成功、自己的计数与 endpoint。
    let bound = {
        let reg = registry::get_registry().lock();
        endpoint::get_endpoints().lock().bind(
            &reg,
            second.endpoint,
            ContractId::from_raw(SVC_CONTRACT),
            InterfaceAbi::from_raw(SVC_ABI),
            ExecutionDomain::KernelNative,
        )
    };
    match bound {
        Ok(bound) if bound.mechanism == Mechanism::Gate => {}
        _ => fail_case("isolated-ready-fault", "fresh bind did not select Gate"),
    }
    let transport = with_kernel_caller(caller, 0x73, || {
        call::endpoint_call(
            second.endpoint,
            SVC_METHOD_ECHO,
            core::ptr::null(),
            0,
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
            &mut out_status,
        )
    });
    if transport != Ok(()) || out_status != SVC_STATUS_OK {
        fail_case("isolated-ready-fault", "fresh instance could not serve");
    }
    let expected = [input[0] ^ SVC_ECHO_XOR, input[1] ^ SVC_ECHO_XOR];
    if output != expected {
        fail_case("isolated-ready-fault", "fresh instance output mismatch");
    }
    if unsafe { svc_slot(second.window_pa, SVC_R_CALLS) } != 1 {
        fail_case(
            "isolated-ready-fault",
            "fresh instance call was not observed",
        );
    }

    // (6) 收尾：新实例优雅停止（destroy 真的跑过）。
    if kernel::component::stop_component(second.id).is_err() {
        fail_case("isolated-ready-fault", "fresh instance stop failed");
    }
    if unsafe { svc_slot(second.window_pa, SVC_R_DESTROY_MAGIC) } != SVC_DESTROY_MAGIC {
        fail_case("isolated-ready-fault", "fresh instance destroy did not run");
    }
    if !kernel_native_still_works() {
        fail_case("isolated-ready-fault", "KernelNative path broke");
    }
    kernel::log!(
        "selftest",
        "isolated-ready-fault: contained + restarted: failed={}, fresh={}",
        first.id.raw(),
        second.id.raw()
    );
    pass("isolated-ready-fault")
}

/// 逻辑重启：前一个实例 create 失败（`Failed` tombstone）之后同名 artifact
/// 创建**全新实例**（同一常驻 image，全新 AS / 窗口 / slot）；两个 tombstone
/// 类型（`Failed` / `Stopped`）都不阻止重启；并发活跃实例显式拒绝。
pub(crate) fn isolated_restart() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::isolated_lifecycle;
    use kernel::component::load::{self, ComponentLoadError};
    use kernel::component::registry;
    use kernel::component::runtime_slot;
    use kernel::component::ComponentState;
    use kernel::errno::Errno;
    use kernel::memory::address_space;

    let core_satp = read_satp();

    // (1) 失败一个实例：create 入口返回 `-EINVAL`（Failed tombstone）。
    let args = KcompCreateArgs {
        config_abi: LIFE_FAIL_ABI,
        config: core::ptr::null(),
        config_len: 0,
    };
    let error = match load::create_component(
        b"kcomp_isolated_life",
        &args,
        ExecutionDomain::IsolatedNative,
    ) {
        Err(error) => error,
        Ok(_) => fail_case("isolated-restart", "fail-config create succeeded"),
    };
    if error != ComponentLoadError::CreateFailed(-22) {
        kernel::log!(
            "selftest",
            "isolated-restart: unexpected create error: {:?}",
            error
        );
        fail_case("isolated-restart", "unexpected create error");
    }
    let (failed_id, failed_handle, failed_image) = match failed_isolated_instance() {
        Some(found) => found,
        None => fail_case("isolated-restart", "no failed Isolated instance"),
    };
    assert_failure_released("isolated-restart", failed_id, failed_handle);

    // (2) 重启：同一 image 的全新实例。
    let second = match load::create_component(
        b"kcomp_isolated_life",
        &KcompCreateArgs::empty(),
        ExecutionDomain::IsolatedNative,
    ) {
        Ok(id) => id,
        Err(error) => {
            kernel::log!("selftest", "isolated-restart: restart failed: {:?}", error);
            fail_case("isolated-restart", "logical restart failed");
        }
    };
    let (second_handle, second_image, second_window_pa) = {
        let reg = registry::get_registry().lock();
        let record = match reg.get(second) {
            Some(record) => record,
            None => fail_case("isolated-restart", "restarted instance record missing"),
        };
        let handle = match record.address_space {
            Some(handle) => handle,
            None => fail_case(
                "isolated-restart",
                "restarted instance has no address space",
            ),
        };
        let window = isolated_lifecycle::window_range();
        let window_pa = match address_space::mapping_exact(handle, &window) {
            Ok(Some(mapping)) => mapping.physical_range.base,
            _ => fail_case(
                "isolated-restart",
                "restarted instance window is not mapped",
            ),
        };
        (handle, record.image, window_pa)
    };
    if second == failed_id {
        fail_case("isolated-restart", "restart reused the ComponentId");
    }
    if second_handle.raw_id() == failed_handle.raw_id() {
        fail_case("isolated-restart", "restart reused the address space");
    }
    if second_image != failed_image {
        fail_case("isolated-restart", "restart did not reuse the image");
    }
    if registry_state(second) != Some(ComponentState::Ready) {
        fail_case("isolated-restart", "restarted instance is not Ready");
    }
    // create 真的在私有 AS 里跑过（窗口上报）。
    if unsafe { life_slot(second_window_pa, LIFE_R_MAGIC) } != LIFE_REPORT_MAGIC {
        fail_case(
            "isolated-restart",
            "restarted create did not run in its private AS",
        );
    }

    // (3) 并发活跃实例显式拒绝（同一 image 的第二个活跃实例）。
    let error = match load::create_component(
        b"kcomp_isolated_life",
        &KcompCreateArgs::empty(),
        ExecutionDomain::IsolatedNative,
    ) {
        Err(error) => error,
        Ok(_) => fail_case(
            "isolated-restart",
            "a concurrent live instance was accepted",
        ),
    };
    if error != ComponentLoadError::IsolatedInstanceLive {
        kernel::log!(
            "selftest",
            "isolated-restart: unexpected concurrency error: {:?}",
            error
        );
        fail_case(
            "isolated-restart",
            "concurrent live instance was not rejected",
        );
    }
    if Errno::from(error) != Errno::EBUSY {
        fail_case("isolated-restart", "concurrency rejection errno mismatch");
    }
    if registry_state(second) != Some(ComponentState::Ready) {
        fail_case(
            "isolated-restart",
            "rejected concurrency changed the live instance",
        );
    }

    // (4) 优雅停止 second（Stopped tombstone、窗口驻留）→ 再次重启。
    if kernel::component::stop_component(second).is_err() {
        fail_case("isolated-restart", "stop failed");
    }
    if unsafe { life_slot(second_window_pa, LIFE_DESTROY_SLOT) } != LIFE_DESTROY_MAGIC {
        fail_case("isolated-restart", "destroy entry did not run");
    }
    let third = match load::create_component(
        b"kcomp_isolated_life",
        &KcompCreateArgs::empty(),
        ExecutionDomain::IsolatedNative,
    ) {
        Ok(id) => id,
        Err(error) => {
            kernel::log!(
                "selftest",
                "isolated-restart: second restart failed: {:?}",
                error
            );
            fail_case("isolated-restart", "restart after Stopped failed");
        }
    };
    let (third_handle, third_window_pa) = {
        let reg = registry::get_registry().lock();
        let record = match reg.get(third) {
            Some(record) => record,
            None => fail_case("isolated-restart", "second restart record missing"),
        };
        let handle = match record.address_space {
            Some(handle) => handle,
            None => fail_case("isolated-restart", "second restart has no address space"),
        };
        let window = isolated_lifecycle::window_range();
        let window_pa = match address_space::mapping_exact(handle, &window) {
            Ok(Some(mapping)) => mapping.physical_range.base,
            _ => fail_case("isolated-restart", "second restart window is not mapped"),
        };
        (handle, window_pa)
    };
    if third == second {
        fail_case("isolated-restart", "second restart reused the ComponentId");
    }
    if third_handle.raw_id() == second_handle.raw_id() {
        fail_case(
            "isolated-restart",
            "second restart reused the address space",
        );
    }
    // **全新窗口 backing**：second 的窗口仍驻留（被占用），third 必须拿到
    // 不同的页——重启的独立性是"全新机制"，不是"复用旧窗口"。
    if third_window_pa == second_window_pa {
        fail_case(
            "isolated-restart",
            "restart reused the resident window backing",
        );
    }
    // fresh slot：旧实例的 slot 已清除，新实例的 slot 已安装。
    let expected_slot =
        isolated_lifecycle::window_range().base + isolated_lifecycle::WINDOW_RUNTIME_OFF;
    if runtime_slot::get_slots().lock().get(third) as usize != expected_slot {
        fail_case("isolated-restart", "second restart has no runtime slot");
    }
    if !runtime_slot::get_slots().lock().get(second).is_null() {
        fail_case("isolated-restart", "stopped instance kept its runtime slot");
    }
    // 两个 tombstone 都被保留，且都不阻止重启。
    if registry_state(failed_id) != Some(ComponentState::Failed) {
        fail_case("isolated-restart", "Failed tombstone was lost");
    }
    if registry_state(second) != Some(ComponentState::Stopped) {
        fail_case("isolated-restart", "Stopped tombstone was lost");
    }

    // 收尾：停止 third（destroy 真的跑过）。
    if kernel::component::stop_component(third).is_err() {
        fail_case("isolated-restart", "third stop failed");
    }
    if unsafe { life_slot(third_window_pa, LIFE_DESTROY_SLOT) } != LIFE_DESTROY_MAGIC {
        fail_case("isolated-restart", "third destroy entry did not run");
    }
    if read_satp() != core_satp {
        fail_case("isolated-restart", "Core satp not restored");
    }
    if !kernel_native_still_works() {
        fail_case("isolated-restart", "KernelNative path broke");
    }
    kernel::log!(
        "selftest",
        "isolated-restart: fresh instances: failed={}, stopped={}, live={}",
        failed_id.raw(),
        second.raw(),
        third.raw()
    );
    pass("isolated-restart")
}

/// 只记录现场、拒绝恢复的窄策略：**组件身份本身不是可恢复的证明**；
/// 断言全部留在 Core（用例）侧。
pub(crate) fn svc_fault_policy(fault: &mut ComponentFault<'_>) -> FaultDecision {
    SVC_FAULT_COUNT.fetch_add(1, Ordering::AcqRel);
    SVC_FAULT_CAUSE.store(fault.cause, Ordering::Release);
    SVC_FAULT_STVAL.store(fault.stval, Ordering::Release);
    FAULT_HANDLER_SATP.store(read_satp(), Ordering::Release);
    FAULT_HANDLER_SP.store(read_sp(), Ordering::Release);
    FaultDecision::Abandon
}

use super::*;
