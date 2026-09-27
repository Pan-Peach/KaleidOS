//! Isolated instance lifecycle through the production entry points:
//! create → Ready → destroy, and the create/destroy failure terminals.

// -----------------------------------------------------------------------
// Isolated 生命周期（生产路径 create → Ready → destroy）。
//
// 夹具 `kcomp_isolated_life` 经 `load::create_component(..., IsolatedNative)`
// 创建：私有 AS + 按域镜像 + Core 预置窗口（栈 / 实例窗口）由 Core 建立，
// `kcomp_instance_create` 经 跨 AS trampoline 在私有 AS 里执行；组件把
// 观察值写进自己的实例窗口，ArchTest 从 Core 视图读回并断言。destroy 同理。
// -----------------------------------------------------------------------

/// `kcomp_isolated_life` 的窗口上报槽号（与组件源码逐槽一致）。
pub(crate) const LIFE_REPORT_OFF: usize = 512;
pub(crate) const LIFE_R_MAGIC: usize = 0;
pub(crate) const LIFE_R_TP: usize = 1;
pub(crate) const LIFE_R_SATP: usize = 2;
pub(crate) const LIFE_R_ARGS: usize = 3;
pub(crate) const LIFE_R_OUT_STATE: usize = 4;
pub(crate) const LIFE_R_CONFIG_ABI: usize = 5;
pub(crate) const LIFE_R_CONFIG_LEN: usize = 6;
pub(crate) const LIFE_R_CONFIG0: usize = 7;
pub(crate) const LIFE_R_CONFIG1: usize = 8;
pub(crate) const LIFE_R_SELF: usize = 9;
/// destroy 标记的**槽号**（= `LIFE_DESTROY_OFF / size_of::<usize>()`）。
pub(crate) const LIFE_DESTROY_SLOT: usize = 10;
pub(crate) const LIFE_R_VIEW_KIND: usize = 11;
pub(crate) const LIFE_R_VIEW_BASE: usize = 12;
pub(crate) const LIFE_R_VIEW_LEN: usize = 13;

pub(crate) const LIFE_REPORT_MAGIC: usize = 0x4C49_4645; // "LIFE"
pub(crate) const LIFE_DESTROY_MAGIC: usize = 0x4C49_4644; // "LIFD"
/// 成功用例传给组件的 config 负载（Core 必须原样拷进实例窗口）。
pub(crate) const LIFE_CONFIG: [u8; 2] = [0xC0, 0xDE];
pub(crate) const LIFE_CONFIG_ABI: u64 = 0x4C49_4645_0001;
/// 故障注入：create 见到这个 config_abi 立即返回 `-EINVAL`。
pub(crate) const LIFE_FAIL_ABI: u64 = 0xDEAD_BEEF;
/// 故障注入：create 见到这个 config_abi 在私有 AS 里执行非法指令。
pub(crate) const LIFE_FAULT_ABI: u64 = 0xDEAD_FA11;
/// 故障注入：create 成功，但 destroy 入口执行非法指令（destroy 故障路径）。
pub(crate) const LIFE_DESTROY_FAULT_ABI: u64 = 0xDEAD_DE57;
/// destroy 故障标记槽（create 写；与组件源码逐槽一致）。
pub(crate) const LIFE_R_DESTROY_FAULT: usize = 14;
/// destroy 进入计数槽（destroy 每次进入先自增）。
pub(crate) const LIFE_R_DESTROY_CALLS: usize = 15;

/// 从实例窗口 backing 的 Core 视图读一个槽（窗口 PA 由 Core 的映射真相给出）。
///
/// # Safety
/// `window_pa` 必须是本用例实例窗口 backing 的基址（仍驻留）。
pub(crate) unsafe fn life_slot(window_pa: usize, index: usize) -> usize {
    unsafe {
        let base = (window_pa + LIFE_REPORT_OFF) as *const usize;
        core::ptr::read_volatile(base.add(index))
    }
}

/// 主用例：生产路径创建 → Ready（create 在私有 AS 里跑过）→ 销毁 → Stopped。
pub(crate) fn isolated_lifecycle() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::isolated_lifecycle::{
        self, ISOLATED_STACK_BASE, WINDOW_OUT_STATE_OFF, WINDOW_RUNTIME_OFF,
    };
    use kernel::component::load;
    use kernel::component::registry;
    use kernel::component::runtime_slot;
    use kernel::component::ComponentState;
    use kernel::memory::address_space::{self, MapError};

    let core_satp = read_satp();
    let args = KcompCreateArgs {
        config_abi: LIFE_CONFIG_ABI,
        config: LIFE_CONFIG.as_ptr() as *const (),
        config_len: LIFE_CONFIG.len(),
    };

    // When：经生产入口创建一个 Isolated 实例。
    let id = match load::create_component(
        b"kcomp_isolated_life",
        &args,
        ExecutionDomain::IsolatedNative,
    ) {
        Ok(id) => id,
        Err(error) => {
            kernel::log!("selftest", "isolated-lifecycle: create failed: {:?}", error);
            fail("isolated-lifecycle: create failed");
        }
    };

    // Then：Core AS 已恢复，实例 Ready，AS 句柄 / state / 镜像都在 Core 真相里。
    if read_satp() != core_satp {
        fail("isolated-lifecycle: Core satp not restored after create");
    }
    let (state, handle, instance_state) = {
        let reg = registry::get_registry().lock();
        let record = match reg.get(id) {
            Some(record) => record,
            None => fail("isolated-lifecycle: instance record missing"),
        };
        (
            record.state,
            record.address_space,
            record.instance_state as usize,
        )
    };
    if state != ComponentState::Ready {
        fail("isolated-lifecycle: instance did not reach Ready");
    }
    let handle = match handle {
        Some(handle) => handle,
        None => fail("isolated-lifecycle: instance has no address space"),
    };

    // 实例窗口的映射真相 + Core 视图（窗口 backing 由 Core 预置）。
    let window = isolated_lifecycle::window_range();
    let window_pa = match address_space::mapping_exact(handle, &window) {
        Ok(Some(mapping)) => mapping.physical_range.base,
        _ => fail("isolated-lifecycle: instance window is not mapped"),
    };

    // (a) 组件写回的 out_state 是**实例内 VA**，指向窗口里的上报区。
    let expected_report = window.base + LIFE_REPORT_OFF;
    if instance_state != expected_report {
        fail("isolated-lifecycle: out_state is not the in-window report address");
    }

    // (b) 上报内容：组件真的在私有 AS 里跑过、args / config / tp 都对得上。
    // SAFETY: 窗口 backing 由 Core 分配且仍驻留；索引都在上报页内。
    let slot = |index: usize| unsafe { life_slot(window_pa, index) };
    if slot(LIFE_R_MAGIC) != LIFE_REPORT_MAGIC {
        fail("isolated-lifecycle: component did not write its window report");
    }
    let expected_satp = match address_space::prepare_activation(handle) {
        Ok(activation) => activation.token().satp(),
        Err(_) => fail("isolated-lifecycle: prepare_activation failed"),
    };
    if slot(LIFE_R_SATP) != expected_satp {
        fail("isolated-lifecycle: component did not observe the private root");
    }
    if expected_satp == core_satp {
        fail("isolated-lifecycle: private root equals the Core root");
    }
    if slot(LIFE_R_ARGS) != window.base {
        fail("isolated-lifecycle: create args were not delivered in the window");
    }
    if slot(LIFE_R_OUT_STATE) != window.base + WINDOW_OUT_STATE_OFF {
        fail("isolated-lifecycle: component did not see its out_state slot");
    }
    if slot(LIFE_R_SELF) == 0 {
        fail("isolated-lifecycle: component self address is zero");
    }
    if slot(LIFE_R_CONFIG_ABI) != LIFE_CONFIG_ABI as usize
        || slot(LIFE_R_CONFIG_LEN) != LIFE_CONFIG.len()
        || slot(LIFE_R_CONFIG0) != LIFE_CONFIG[0] as usize
        || slot(LIFE_R_CONFIG1) != LIFE_CONFIG[1] as usize
    {
        fail("isolated-lifecycle: config payload was not delivered");
    }
    // (b2) Core 预交付的域视图：kind = LOCAL_VA、base/len = 本实例窗口
    //      （表示是实例内 VA，绝不是物理地址 / Core 私有 VA）。
    use kernel::generated::abi::KCORE_MEMORY_VIEW_LOCAL_VA;
    if slot(LIFE_R_VIEW_KIND) != KCORE_MEMORY_VIEW_LOCAL_VA as usize {
        fail("isolated-lifecycle: window view is not LOCAL_VA");
    }
    if slot(LIFE_R_VIEW_BASE) != window.base || slot(LIFE_R_VIEW_LEN) != window.size {
        fail("isolated-lifecycle: window view does not describe the instance window");
    }
    // (c) runtime context：`tp` 就是 Core 为该实例安装的实例内 slot。
    if slot(LIFE_R_TP) != window.base + WINDOW_RUNTIME_OFF {
        fail("isolated-lifecycle: per-instance runtime slot (tp) not installed");
    }
    if runtime_slot::get_slots().lock().get(id) as usize != window.base + WINDOW_RUNTIME_OFF {
        fail("isolated-lifecycle: runtime slot table disagrees with the window");
    }

    // (d) 窗口只属于本实例：另一个 AS 不映射这个 VA，窗口 VA 也不在 Core 的
    //     恒等映射 RAM 窗口里（Core AS 看不到它）。
    let other =
        match address_space::create_isolated_address_space_for(ComponentId::from_raw(0x1A5E)) {
            Ok(other) => other,
            Err(_) => fail("isolated-lifecycle: second address space creation failed"),
        };
    if !matches!(address_space::translate(other, window.base), Ok(None)) {
        fail("isolated-lifecycle: instance window is reachable from another AS");
    }
    if !matches!(
        address_space::translate(other, ISOLATED_STACK_BASE),
        Ok(None)
    ) {
        fail("isolated-lifecycle: component stack is reachable from another AS");
    }
    let _ = address_space::retire(other);
    if window.base >= 0x8000_0000 {
        fail("isolated-lifecycle: window VA overlaps the Core RAM identity window");
    }

    // When：优雅停止（生产路径）。
    if let Err(error) = kernel::component::stop_component(id) {
        kernel::log!("selftest", "isolated-lifecycle: stop failed: {:?}", error);
        fail("isolated-lifecycle: stop failed");
    }

    // Then：Core AS 恢复、Stopped、destroy 入口真的执行过、AS 已退役。
    if read_satp() != core_satp {
        fail("isolated-lifecycle: Core satp not restored after destroy");
    }
    if registry::get_registry().lock().get(id).map(|r| r.state) != Some(ComponentState::Stopped) {
        fail("isolated-lifecycle: instance did not reach Stopped");
    }
    // SAFETY: 窗口 backing 在销毁后仍驻留（逻辑死亡 / 物理驻留）。
    if slot(LIFE_DESTROY_SLOT) != LIFE_DESTROY_MAGIC {
        fail("isolated-lifecycle: destroy entry did not run");
    }
    match address_space::prepare_activation(handle) {
        Err(MapError::Retired) => {}
        _ => fail("isolated-lifecycle: address space was not retired"),
    }
    kernel::log!(
        "selftest",
        "isolated-lifecycle: private AS OK: id={}, window={:#x}, satp={:#x}",
        id.raw(),
        window.base,
        expected_satp
    );
    pass("isolated-lifecycle")
}

/// 带用例名的失败出口：日志写清是哪个用例的哪条不变量，再走统一的 FAIL。
pub(crate) fn fail_case(case: &str, reason: &'static str) -> ! {
    kernel::log!("selftest", "{}: {}", case, reason);
    fail(reason)
}

/// 实例的 Core 真相状态（不存在 = `None`）。
pub(crate) fn registry_state(id: ComponentId) -> Option<kernel::component::ComponentState> {
    kernel::component::registry::get_registry()
        .lock()
        .get(id)
        .map(|record| record.state)
}

/// 最近一个 `Failed` 的 Isolated 组件（id / AS 句柄）；扫描是 ArchTest
/// 的观察手段（create 失败时调用方拿不到 id）。
pub(crate) fn failed_isolated_instance() -> Option<(ComponentId, AddressSpaceHandle)> {
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::registry;
    use kernel::component::ComponentState;

    let reg = registry::get_registry().lock();
    let mut found = None;
    for record in reg.iter() {
        if record.execution_domain == ExecutionDomain::IsolatedNative
            && record.state == ComponentState::Failed
        {
            if let Some(handle) = record.address_space {
                found = Some((record.id, handle));
            }
        }
    }
    found
}

/// 失败清理断言（**Core 中止实例**的路径：create / service 故障）：
/// `Failed` + AS 退役 + Core 预置窗口（栈 / 实例窗口）归还 backing +
/// runtime slot 清除。
pub(crate) fn assert_failure_released(case: &str, id: ComponentId, handle: AddressSpaceHandle) {
    use kernel::component::isolated_lifecycle;
    use kernel::component::runtime_slot;
    use kernel::component::ComponentState;
    use kernel::memory::address_space::{self, MapError};

    if registry_state(id) != Some(ComponentState::Failed) {
        fail_case(case, "instance was not marked Failed");
    }
    match address_space::prepare_activation(handle) {
        Err(MapError::Retired) => {}
        _ => fail_case(case, "address space was not retired"),
    }
    for range in [
        isolated_lifecycle::stack_range(),
        isolated_lifecycle::window_range(),
    ] {
        if !matches!(address_space::mapping_exact(handle, &range), Ok(None)) {
            fail_case(case, "a Core-prepared window leaked");
        }
    }
    if !runtime_slot::get_slots().lock().get(id).is_null() {
        fail_case(case, "runtime slot was not cleared");
    }
}

/// destroy 路径断言（成功或入口故障）：AS 退役 + Core 预置窗口**保持驻留**
/// （AS 退役后不可再进入）+ runtime slot 清除。
pub(crate) fn assert_destroy_path_retired(
    case: &str,
    id: ComponentId,
    handle: AddressSpaceHandle,
    window: &VirtualRange,
) {
    use kernel::component::isolated_lifecycle;
    use kernel::component::runtime_slot;
    use kernel::memory::address_space::{self, MapError};

    match address_space::prepare_activation(handle) {
        Err(MapError::Retired) => {}
        _ => fail_case(case, "address space was not retired"),
    }
    if !matches!(address_space::mapping_exact(handle, window), Ok(Some(_))) {
        fail_case(case, "destroy path must keep the prepared window resident");
    }
    if !matches!(
        address_space::mapping_exact(handle, &isolated_lifecycle::stack_range()),
        Ok(Some(_))
    ) {
        fail_case(case, "destroy path must keep the prepared window resident");
    }
    if !runtime_slot::get_slots().lock().get(id).is_null() {
        fail_case(case, "runtime slot was not cleared");
    }
}

/// KernelNative 路径不受影响：`kcomp_smoke` 仍能创建到 `Ready`。
///
/// "Core stays alive / KernelNative unaffected"不变式：Isolated
/// 失败不得污染共享 AS 的生命周期链。
pub(crate) fn kernel_native_still_works() -> bool {
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::load;
    use kernel::component::registry;
    use kernel::component::ComponentState;

    match load::load_and_start(b"kcomp_smoke", ExecutionDomain::KernelNative) {
        Ok(id) => {
            registry::get_registry()
                .lock()
                .get(id)
                .map(|record| record.state)
                == Some(ComponentState::Ready)
        }
        Err(error) => {
            kernel::log!("selftest", "kernel_native_still_works: {:?}", error);
            false
        }
    }
}

/// create 失败的公共终态断言：实例留 tombstone（`Failed`）、AS 退役、
/// Core 预置窗口 / 栈归还、runtime slot 清空（半成品不留）。
pub(crate) fn assert_failed_isolated_cleanup() {
    match failed_isolated_instance() {
        Some((id, handle)) => assert_failure_released("isolated create failure", id, handle),
        None => fail("isolated create failure: no failed Isolated instance with an AS"),
    }
}

/// 失败路径（create 返回非零）：Failed + AS 退役 + Core 预置窗口归还，
/// 不留半成品实例。
pub(crate) fn isolated_lifecycle_fail() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::load::{self, ComponentLoadError};

    let core_satp = read_satp();
    let args = KcompCreateArgs {
        config_abi: LIFE_FAIL_ABI,
        config: core::ptr::null(),
        config_len: 0,
    };

    // When：create 在组件入口里失败（-EINVAL）。
    let error = match load::create_component(
        b"kcomp_isolated_life",
        &args,
        ExecutionDomain::IsolatedNative,
    ) {
        Err(error) => error,
        Ok(_) => fail("isolated-lifecycle-fail: create succeeded with the fail config"),
    };
    if error != ComponentLoadError::CreateFailed(-22) {
        kernel::log!(
            "selftest",
            "isolated-lifecycle-fail: unexpected create error: {:?}",
            error
        );
        fail("isolated-lifecycle-fail: unexpected create error");
    }
    if read_satp() != core_satp {
        fail("isolated-lifecycle-fail: Core satp not restored");
    }

    // Then：tombstone + 清理（公共断言）+ KernelNative 路径不受影响。
    assert_failed_isolated_cleanup();
    if !kernel_native_still_works() {
        fail("isolated-lifecycle-fail: KernelNative path broke after the failure");
    }
    pass("isolated-lifecycle-fail")
}

/// 失败路径（create 在私有 AS 里故障）：普通 trap 路径的故障分派判不可恢复
/// （`Outcome::Faulted`）→ `CreateFaulted` + 同一套清理，绝不把 Core 打 panic。
pub(crate) fn isolated_lifecycle_fault() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::load::{self, ComponentLoadError};

    let core_satp = read_satp();
    let args = KcompCreateArgs {
        config_abi: LIFE_FAULT_ABI,
        config: core::ptr::null(),
        config_len: 0,
    };

    // When：create 在私有 AS 里执行非法指令。
    let error = match load::create_component(
        b"kcomp_isolated_life",
        &args,
        ExecutionDomain::IsolatedNative,
    ) {
        Err(error) => error,
        Ok(_) => fail("isolated-lifecycle-fault: create succeeded with the fault config"),
    };
    if error != ComponentLoadError::CreateFaulted {
        kernel::log!(
            "selftest",
            "isolated-lifecycle-fault: unexpected create error: {:?}",
            error
        );
        fail("isolated-lifecycle-fault: unexpected create error");
    }
    if read_satp() != core_satp {
        fail("isolated-lifecycle-fault: Core satp not restored after the fault");
    }

    // Then：tombstone + 清理（与返回非零同一路径）+ KernelNative 不受影响。
    assert_failed_isolated_cleanup();
    if !kernel_native_still_works() {
        fail("isolated-lifecycle-fault: KernelNative path broke after the fault");
    }
    pass("isolated-lifecycle-fault")
}

use super::*;
