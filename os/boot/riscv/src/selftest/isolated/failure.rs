//! Failure-matrix cases: placement / config / prepare rejections, destroy-entry
//! fault, and stale-endpoint access blocking.

// -----------------------------------------------------------------------
// 失败 / 重启矩阵。
//
// 逐条证明"组件失败 = 逻辑死亡、物理驻留"：每个阶段失败之后实例状态 / AS /
// Core 预置窗口 / runtime slot / endpoint / caller 错误 / Core 存活 /
// KernelNative 不受影响都有可观察断言；失败之后同一 image 可以**逻辑重启**
// （全新实例、全新 AS / 窗口 / slot）。
// -----------------------------------------------------------------------

/// 装载拒绝：放段失败 / import 包络在**声明实例之前**显式拒绝，不留实例 /
/// AS / image；KernelNative 路径不受影响。
pub(crate) fn isolated_load_reject() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::image;
    use kernel::component::load::{self, ComponentLoadError};
    use kernel::component::registry;
    use kernel::errno::Errno;

    let core_satp = read_satp();
    let instances_before = registry::get_registry().lock().iter().count();
    let images_before = image::get_images().lock().len();

    // (a) 按域放段失败：17 MiB `.bss` 段超出实例镜像窗口（16 MiB）。
    let error = match load::create_component(
        b"kcomp_isolated_bad",
        &KcompCreateArgs::empty(),
        ExecutionDomain::IsolatedNative,
    ) {
        Err(error) => error,
        Ok(_) => fail_case("isolated-load-reject", "oversized artifact was accepted"),
    };
    if error != ComponentLoadError::IsolatedPlacementFailed {
        kernel::log!(
            "selftest",
            "isolated-load-reject: unexpected placement error: {:?}",
            error
        );
        fail_case(
            "isolated-load-reject",
            "oversized artifact was not rejected at placement",
        );
    }
    if Errno::from(error) != Errno::EINVAL {
        fail_case("isolated-load-reject", "placement rejection errno mismatch");
    }

    // (b) import 包络 = 空集：真实组件的 `kcore_*` import 在装载前拒绝。
    let error = match load::create_component(
        b"kcomp_smoke",
        &KcompCreateArgs::empty(),
        ExecutionDomain::IsolatedNative,
    ) {
        Err(error) => error,
        Ok(_) => fail_case("isolated-load-reject", "kcore import artifact was accepted"),
    };
    if error != ComponentLoadError::IsolatedImportUnsupported {
        kernel::log!(
            "selftest",
            "isolated-load-reject: unexpected import error: {:?}",
            error
        );
        fail_case(
            "isolated-load-reject",
            "kcore import was not rejected before loading",
        );
    }
    if Errno::from(error) != Errno::ENOTSUP {
        fail_case("isolated-load-reject", "import rejection errno mismatch");
    }

    // 两条拒绝都发生在声明 / 放段 / 登记之前：Core 真相零副作用。
    if read_satp() != core_satp {
        fail_case(
            "isolated-load-reject",
            "Core satp changed on a rejected load",
        );
    }
    if registry::get_registry().lock().iter().count() != instances_before {
        fail_case(
            "isolated-load-reject",
            "a rejected load declared an instance",
        );
    }
    if image::get_images().lock().len() != images_before {
        fail_case(
            "isolated-load-reject",
            "a rejected load registered an image",
        );
    }

    // KernelNative 路径不受影响：同一 artifact 仍能正常创建到 Ready。
    if !kernel_native_still_works() {
        fail_case(
            "isolated-load-reject",
            "KernelNative path broke after rejected Isolated loads",
        );
    }
    kernel::log!(
        "selftest",
        "isolated-load-reject: rejected before declare: instances={}",
        instances_before
    );
    pass("isolated-load-reject")
}

/// config 负载拒绝：超过窗口固定区的 config 在 create 入口执行**之前**显式
/// 拒绝；实例 Failed + AS 退役 + 预置窗口归还（半成品不留）。
pub(crate) fn isolated_config_reject() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::isolated_lifecycle::WINDOW_CONFIG_MAX;
    use kernel::component::load::{self, ComponentLoadError};
    use kernel::errno::Errno;

    let core_satp = read_satp();
    let oversized = [0u8; WINDOW_CONFIG_MAX + 1];
    let args = KcompCreateArgs {
        config_abi: 0x71C0_11EC,
        config: oversized.as_ptr().cast(),
        config_len: oversized.len(),
    };
    let error = match load::create_component(
        b"kcomp_isolated_life",
        &args,
        ExecutionDomain::IsolatedNative,
    ) {
        Err(error) => error,
        Ok(_) => fail_case("isolated-config-reject", "oversized config was accepted"),
    };
    if error != ComponentLoadError::IsolatedConfigRejected {
        kernel::log!(
            "selftest",
            "isolated-config-reject: unexpected error: {:?}",
            error
        );
        fail_case(
            "isolated-config-reject",
            "oversized config was not rejected",
        );
    }
    if Errno::from(error) != Errno::EINVAL {
        fail_case("isolated-config-reject", "config rejection errno mismatch");
    }
    if read_satp() != core_satp {
        fail_case("isolated-config-reject", "Core satp not restored");
    }
    // 实例在 create 入口执行之前就失败：tombstone + AS 退役 + 窗口归还 +
    // slot 清除（与 create 失败同一套清理）。
    assert_failed_isolated_cleanup();
    if !kernel_native_still_works() {
        fail_case(
            "isolated-config-reject",
            "KernelNative path broke after the failure",
        );
    }
    pass("isolated-config-reject")
}

/// prepare 失败：机制层的 `isolated::prepare` 在入口不可执行 / 栈不可写 /
/// AS 已退役时**显式拒绝**，不改变实例真相（实例仍 Ready、AS 仍可激活、
/// slot 原样、Core satp 不变）。
///
/// 生产 create 路径的 prepare 失败与其它 create 失败共用 `fail_with_as` 清理
/// （已由 create-entry 故障用例证明）；本用例钉住拒绝判据本身 + "拒绝不动
/// 真相"。
pub(crate) fn isolated_prepare_reject() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::image;
    use kernel::component::isolated::{self, IsolatedPrepareError};
    use kernel::component::isolated_lifecycle;
    use kernel::component::load;
    use kernel::component::registry;
    use kernel::component::runtime_slot;
    use kernel::component::ComponentState;
    use kernel::memory::address_space::{self, MapError, VirtualRange};

    let core_satp = read_satp();
    let id = match load::create_component(
        b"kcomp_isolated_life",
        &KcompCreateArgs::empty(),
        ExecutionDomain::IsolatedNative,
    ) {
        Ok(id) => id,
        Err(_) => fail_case("isolated-prepare-reject", "instance create failed"),
    };
    let (handle, image_id) = {
        let reg = registry::get_registry().lock();
        match reg.get(id) {
            Some(record) => match record.address_space {
                Some(handle) => (handle, record.image),
                None => fail_case("isolated-prepare-reject", "instance has no address space"),
            },
            None => fail_case("isolated-prepare-reject", "instance record missing"),
        }
    };
    let create_entry = match image::get_images()
        .lock()
        .get(image_id)
        .map(|record| record.create)
    {
        Some(entry) => entry,
        None => fail_case("isolated-prepare-reject", "image is missing"),
    };
    let stack = isolated_lifecycle::stack_range();
    let window = isolated_lifecycle::window_range();
    let slot = window.base + isolated_lifecycle::WINDOW_RUNTIME_OFF;

    // (a) 入口不在可执行映射：实例窗口是 R+W（不可执行）。
    match isolated::prepare(
        handle,
        window.base,
        stack,
        slot,
        false,
        isolated::EntryArgs::pair(0, 0),
    ) {
        Err(IsolatedPrepareError::EntryNotExecutable) => {}
        _ => fail_case(
            "isolated-prepare-reject",
            "non-executable entry was not rejected",
        ),
    }
    // (b) 栈不被可写映射覆盖：未映射 VA 区间。
    let unmapped_stack = VirtualRange {
        base: 0x4000_0000,
        size: 4096,
    };
    match isolated::prepare(
        handle,
        create_entry,
        unmapped_stack,
        slot,
        false,
        isolated::EntryArgs::pair(0, 0),
    ) {
        Err(IsolatedPrepareError::StackNotWritable) => {}
        _ => fail_case("isolated-prepare-reject", "unmapped stack was not rejected"),
    }

    // 实例真相不变：仍 Ready、AS 仍可激活、slot 原样、Core satp 不变。
    if registry_state(id) != Some(ComponentState::Ready) {
        fail_case(
            "isolated-prepare-reject",
            "instance left Ready after rejected prepares",
        );
    }
    if address_space::prepare_activation(handle).is_err() {
        fail_case("isolated-prepare-reject", "address space became unusable");
    }
    if runtime_slot::get_slots().lock().get(id) as usize != slot {
        fail_case("isolated-prepare-reject", "runtime slot changed");
    }
    if read_satp() != core_satp {
        fail_case("isolated-prepare-reject", "Core satp changed");
    }

    // (c) 退役 AS 拒绝使用：优雅停止之后同一 prepare 得到 `Retired`。
    if kernel::component::stop_component(id).is_err() {
        fail_case("isolated-prepare-reject", "stop failed");
    }
    match isolated::prepare(
        handle,
        create_entry,
        stack,
        slot,
        false,
        isolated::EntryArgs::pair(0, 0),
    ) {
        Err(IsolatedPrepareError::Retired) => {}
        _ => fail_case(
            "isolated-prepare-reject",
            "retired address space accepted a prepare",
        ),
    }
    match address_space::prepare_activation(handle) {
        Err(MapError::Retired) => {}
        _ => fail_case("isolated-prepare-reject", "address space was not retired"),
    }
    // 退役 AS 拒绝任何**新的落映射**（复用 = 新建空间）：stale 资源不能
    // 悄悄接受新用途。
    let stale_mapping = Mapping {
        virtual_range: VirtualRange {
            base: 0x4100_0000,
            size: 4096,
        },
        physical_range: PhysicalRange {
            base: 0x4100_0000,
            size: 4096,
        },
        permission: MappingPermission::READ,
    };
    match address_space::map(handle, stale_mapping) {
        Err(MapError::Retired) => {}
        _ => fail_case(
            "isolated-prepare-reject",
            "retired address space accepted a mapping",
        ),
    }
    if !kernel_native_still_works() {
        fail_case("isolated-prepare-reject", "KernelNative path broke");
    }
    kernel::log!(
        "selftest",
        "isolated-prepare-reject: typed rejections held: id={}",
        id.raw()
    );
    pass("isolated-prepare-reject")
}

/// destroy 入口故障：`stop_component` → destroy 在私有 AS 里 trap →
/// `DestroyPanicked`（EIO）、实例 `Failed`、AS 退役、Core 预置窗口保持驻留
/// （与优雅停止同一纪律）、runtime slot 清除；**绝不自动重试析构**
/// （第二次 stop 被状态机拒绝，destroy 计数不变）。
pub(crate) fn isolated_destroy_fault() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::isolated_lifecycle;
    use kernel::component::load;
    use kernel::component::registry;
    use kernel::component::ComponentStopError;
    use kernel::errno::Errno;
    use kernel::memory::address_space;

    let core_satp = read_satp();
    let args = KcompCreateArgs {
        config_abi: LIFE_DESTROY_FAULT_ABI,
        config: core::ptr::null(),
        config_len: 0,
    };
    let id = match load::create_component(
        b"kcomp_isolated_life",
        &args,
        ExecutionDomain::IsolatedNative,
    ) {
        Ok(id) => id,
        Err(error) => {
            kernel::log!(
                "selftest",
                "isolated-destroy-fault: create failed: {:?}",
                error
            );
            fail_case("isolated-destroy-fault", "instance create failed");
        }
    };
    let (handle, window_pa) = {
        let reg = registry::get_registry().lock();
        let handle = match reg.get(id).and_then(|record| record.address_space) {
            Some(handle) => handle,
            None => fail_case("isolated-destroy-fault", "instance has no address space"),
        };
        let window = isolated_lifecycle::window_range();
        let window_pa = match address_space::mapping_exact(handle, &window) {
            Ok(Some(mapping)) => mapping.physical_range.base,
            _ => fail_case("isolated-destroy-fault", "instance window is not mapped"),
        };
        (handle, window_pa)
    };
    // create 真的在私有 AS 里跑过，并且标记了 destroy 故障。
    if unsafe { life_slot(window_pa, LIFE_R_DESTROY_FAULT) } != 1 {
        fail_case("isolated-destroy-fault", "destroy-fault marker missing");
    }

    // When：优雅停止（destroy 入口在私有 AS 里 trap）。
    let result = kernel::component::stop_component(id);
    if result != Err(ComponentStopError::DestroyPanicked) {
        kernel::log!(
            "selftest",
            "isolated-destroy-fault: unexpected stop result: {:?}",
            result
        );
        fail_case(
            "isolated-destroy-fault",
            "destroy fault was not reported as DestroyPanicked",
        );
    }
    if Errno::from(ComponentStopError::DestroyPanicked) != Errno::EIO {
        fail_case("isolated-destroy-fault", "destroy fault errno mismatch");
    }
    if read_satp() != core_satp {
        fail_case(
            "isolated-destroy-fault",
            "Core satp not restored after the fault",
        );
    }

    // Then：Failed + AS 退役 + 窗口驻留 + slot 清除。
    if registry_state(id) != Some(kernel::component::ComponentState::Failed) {
        fail_case("isolated-destroy-fault", "instance was not marked Failed");
    }
    assert_destroy_path_retired(
        "isolated-destroy-fault",
        id,
        handle,
        &isolated_lifecycle::window_range(),
    );
    // destroy 入口进入过**恰好一次**（故障前自增的计数）。
    if unsafe { life_slot(window_pa, LIFE_R_DESTROY_CALLS) } != 1 {
        fail_case(
            "isolated-destroy-fault",
            "destroy entry did not run exactly once",
        );
    }

    // 绝不自动重试析构：第二次 stop 被状态机拒绝，destroy 计数不变。
    if kernel::component::stop_component(id) != Err(ComponentStopError::NotReady) {
        fail_case(
            "isolated-destroy-fault",
            "a failed instance accepted a second stop",
        );
    }
    if unsafe { life_slot(window_pa, LIFE_R_DESTROY_CALLS) } != 1 {
        fail_case(
            "isolated-destroy-fault",
            "destroy was retried after a fault",
        );
    }
    if !kernel_native_still_works() {
        fail_case("isolated-destroy-fault", "KernelNative path broke");
    }
    kernel::log!(
        "selftest",
        "isolated-destroy-fault: contained: id={}, destroy_calls=1",
        id.raw()
    );
    pass("isolated-destroy-fault")
}

/// stale 访问阻断：优雅停止后（`Stopped` tombstone、AS 退役、窗口驻留）用
/// **已解析**的 endpoint 再调用 → Core 边界在 `resolve` 处拒绝
/// （`EndpointDead` / ENOENT），provider **从未再次执行**（窗口计数不变），
/// AS 仍退役、inflight 未泄漏、Core satp 不变。
pub(crate) fn isolated_stale_access() -> ! {
    use kernel::component::abi::InterfaceAbi;
    use kernel::component::call::{self, CallError};
    use kernel::component::endpoint::{
        self, ContractId, EndpointError, ExecutionDomain, Mechanism,
    };
    use kernel::component::isolated_lifecycle;
    use kernel::component::load;
    use kernel::component::registry;
    use kernel::component::ComponentState;
    use kernel::errno::Errno;
    use kernel::memory::address_space::{self, MapError};

    let core_satp = read_satp();
    let provider = svc_provider();
    let caller = match load::load_and_start(b"kcomp_smoke", ExecutionDomain::KernelNative) {
        Ok(id) => id,
        Err(_) => fail_case("isolated-stale-access", "caller load failed"),
    };
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
        _ => fail_case("isolated-stale-access", "bind did not select Gate"),
    }

    // 先成功服务一次：实例健康、Ready。
    let input = [0x11u8, 0x22];
    let mut output = [0u8; 2];
    let mut out_status = 0i32;
    let transport = with_kernel_caller(caller, 0x6D, || {
        call::endpoint_call(
            provider.endpoint,
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
        fail_case("isolated-stale-access", "healthy call failed");
    }
    let calls_before = unsafe { svc_slot(provider.window_pa, SVC_R_CALLS) };

    // When：优雅停止（生产路径；AS 退役、窗口驻留、endpoint 永久失效）。
    if kernel::component::stop_component(provider.id).is_err() {
        fail_case("isolated-stale-access", "stop failed");
    }
    if registry_state(provider.id) != Some(ComponentState::Stopped) {
        fail_case("isolated-stale-access", "instance did not reach Stopped");
    }
    match address_space::prepare_activation(provider.handle) {
        Err(MapError::Retired) => {}
        _ => fail_case("isolated-stale-access", "address space was not retired"),
    }
    if !matches!(
        address_space::mapping_exact(provider.handle, &isolated_lifecycle::window_range()),
        Ok(Some(_))
    ) {
        fail_case(
            "isolated-stale-access",
            "destroy path must keep the prepared window resident",
        );
    }

    // Then：stale endpoint 被 Core 边界拒绝，provider 从未再次执行。
    let transport = with_kernel_caller(caller, 0x6E, || {
        call::endpoint_call(
            provider.endpoint,
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
                "isolated-stale-access: stale call was not blocked: {:?}",
                other
            );
            fail_case("isolated-stale-access", "stale endpoint was not blocked");
        }
    }
    if Errno::from(CallError::Endpoint(EndpointError::EndpointDead)) != Errno::ENOENT {
        fail_case("isolated-stale-access", "stale rejection errno mismatch");
    }
    if unsafe { svc_slot(provider.window_pa, SVC_R_CALLS) } != calls_before {
        fail_case("isolated-stale-access", "provider ran for a stale call");
    }
    if registry::get_registry().lock().active_calls(provider.id) != 0 {
        fail_case("isolated-stale-access", "stale call leaked inflight");
    }
    if read_satp() != core_satp {
        fail_case("isolated-stale-access", "Core satp not restored");
    }
    if !kernel_native_still_works() {
        fail_case("isolated-stale-access", "KernelNative path broke");
    }
    kernel::log!(
        "selftest",
        "isolated-stale-access: blocked: id={}, calls={}",
        provider.id.raw(),
        calls_before
    );
    pass("isolated-stale-access")
}

/// Ready 期故障 + 逻辑重启：实例**已经成功服务过调用**之后在 dispatch 里
/// 故障 → 同一套 containment（Failed + AS 退役 + 窗口归还 + endpoint 永久
/// 失效）；随后 stale endpoint 被拒绝；同一 image 创建的全新实例真正独立
use super::*;
