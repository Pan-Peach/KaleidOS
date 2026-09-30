//! Isolated **直接 Core import** 与 panic escape（CP5 的端到端证明）。
//!
//! - `kcomp_isolated_direct` 只 import 支持面内的诊断 / 只读查询与
//!   `kcore_panic_escape`：装载时重定位到 Core 导出的低别名（共享映射），
//!   运行时是普通 C-ABI 调用，`satp` 保持实例 root。
//! - `kcomp_isolated_unsupported` import 明确不在支持面内的
//!   `kcore_memory_acquire`：装载前显式拒绝（`isolated-load-reject` 覆盖）。
//! - `PANIC_ABI` 触发组件 `panic!`：SDK panic adapter → `kcore_panic_escape`
//!   → 跨 AS 延续（`Outcome::Faulted`）→ 实例 `Failed` + 窗口归还。

/// 直接 import 夹具的上报区偏移（与组件源码一致；窗口布局：args=0、out_state=32、
/// config=64、report=512）。
const DIRECT_REPORT_OFF: usize = 512;

/// `kcomp_isolated_direct` 上报槽号。
const R_MAGIC: usize = 0;
const R_NOW: usize = 1;
const R_CPUS: usize = 2;
const R_COMPONENTS: usize = 3;
const R_FREE: usize = 4;
const R_SATP: usize = 5;
const R_TP: usize = 6;
const R_LOG: usize = 7;

const DIRECT_MAGIC: usize = 0x4449_5245; // "DIRE"
/// 故障注入：create 见到这个 config_abi 就 `panic!`。
const PANIC_ABI: u64 = 0xDEAD_F00D;

/// 从实例窗口 backing 的 Core 视图读一个上报槽。
///
/// # Safety
///
/// `window_pa` 必须是本用例实例窗口 backing 的基址（仍驻留）。
unsafe fn direct_slot(window_pa: usize, index: usize) -> usize {
    unsafe {
        let base = (window_pa + DIRECT_REPORT_OFF) as *const usize;
        core::ptr::read_volatile(base.add(index))
    }
}

/// 直接 Core import：创建 `kcomp_isolated_direct` → Ready → 组件在私有 AS 里
/// 直接调用支持面内的 Core 导出 → 上报（`satp` = 实例 root）→ destroy 执行。
pub(crate) fn isolated_direct_imports() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::isolated_lifecycle;
    use kernel::component::load;
    use kernel::component::registry;
    use kernel::component::ComponentState;

    let core_satp = read_satp();
    let id = match load::create_component(
        b"kcomp_isolated_direct",
        &KcompCreateArgs::empty(),
        ExecutionDomain::IsolatedNative,
    ) {
        Ok(id) => id,
        Err(error) => {
            kernel::log!(
                "selftest",
                "isolated-direct-imports: create failed: {:?}",
                error
            );
            fail("isolated-direct-imports: create failed");
        }
    };
    if registry_state(id) != Some(ComponentState::Ready) {
        fail("isolated-direct-imports: instance is not Ready");
    }

    // Core 侧读回实例窗口 backing（Core root 的 identity 视图）；窗口 VA 属于该实例。
    let handle = match registry::get_registry()
        .lock()
        .get(id)
        .and_then(|record| record.address_space)
    {
        Some(handle) => handle,
        None => fail("isolated-direct-imports: no address space recorded"),
    };
    let window = isolated_lifecycle::window_range();
    let window_pa = match address_space::mapping_exact(handle, &window) {
        Ok(Some(mapping)) => mapping.physical_range.base,
        _ => fail("isolated-direct-imports: instance window missing"),
    };
    let slot = |index: usize| unsafe { direct_slot(window_pa, index) };

    if slot(R_MAGIC) != DIRECT_MAGIC {
        fail("isolated-direct-imports: component did not report");
    }
    if slot(R_NOW) == 0 {
        fail("isolated-direct-imports: kcore_now returned zero");
    }
    if slot(R_CPUS) == 0 {
        fail("isolated-direct-imports: kcore_machine_cpu_count returned zero");
    }
    if slot(R_COMPONENTS) == 0 {
        fail("isolated-direct-imports: kcore_component_count returned zero");
    }
    if slot(R_FREE) == 0 {
        fail("isolated-direct-imports: kcore_free_page_count returned zero");
    }
    // 直接调用发生在实例 root 上：组件观察到的 satp = 实例 root（!= Core root）。
    let expected_satp = match address_space::prepare_activation(handle) {
        Ok(activation) => activation.token().satp(),
        Err(_) => fail("isolated-direct-imports: prepare_activation failed"),
    };
    if slot(R_SATP) != expected_satp || expected_satp == core_satp {
        fail("isolated-direct-imports: direct call did not run on the instance root");
    }
    if slot(R_TP) != 0 {
        fail("isolated-direct-imports: fresh entry did not observe tp == 0");
    }

    // 优雅停止：destroy 入口执行（写 R_LOG）后只退役 AS；窗口 backing 驻留。
    if kernel::component::stop_component(id).is_err() {
        fail("isolated-direct-imports: stop failed");
    }
    if slot(R_LOG) != 1 {
        fail("isolated-direct-imports: destroy entry did not run");
    }
    if read_satp() != core_satp {
        fail("isolated-direct-imports: Core satp not restored");
    }
    if !kernel_native_still_works() {
        fail("isolated-direct-imports: KernelNative path broke");
    }
    kernel::log!(
        "selftest",
        "isolated-direct-imports: direct Core calls OK: now={}, cpus={}, components={}, free={}",
        slot(R_NOW),
        slot(R_CPUS),
        slot(R_COMPONENTS),
        slot(R_FREE)
    );
    pass("isolated-direct-imports")
}

/// 组件 panic 的跨 AS 收敛：`PANIC_ABI` → 组件 `panic!` → SDK adapter
/// （`kcore_log_line` + `kcore_panic_escape`）→ 跨 AS 延续 → `CreateFaulted`、
/// 实例 `Failed`、AS 退役、窗口归还，Core / KernelNative 不受影响。
pub(crate) fn isolated_panic_escape() -> ! {
    use kernel::component::containment::KcompCreateArgs;
    use kernel::component::endpoint::ExecutionDomain;
    use kernel::component::load::{self, ComponentLoadError};

    let core_satp = read_satp();
    let args = KcompCreateArgs {
        config_abi: PANIC_ABI,
        config: core::ptr::null(),
        config_len: 0,
    };
    // panic 放弃路径（跨 AS trampoline 交回挂起调用者）必须恢复调用者的 `tp`：
    // 非零哨兵值钉住它（本镜像无 TLS，tp 是普通执行状态）。
    const CALLER_TP: usize = 0x707B;
    set_tp(CALLER_TP);
    let error = match load::create_component(
        b"kcomp_isolated_direct",
        &args,
        ExecutionDomain::IsolatedNative,
    ) {
        Ok(_) => fail("isolated-panic-escape: panicking create was accepted"),
        Err(error) => error,
    };
    if read_tp() != CALLER_TP {
        fail("isolated-panic-escape: caller tp not restored");
    }
    set_tp(0);
    if error != ComponentLoadError::CreateFaulted {
        kernel::log!(
            "selftest",
            "isolated-panic-escape: unexpected error: {:?}",
            error
        );
        fail("isolated-panic-escape: expected CreateFaulted");
    }
    if read_satp() != core_satp {
        fail("isolated-panic-escape: Core satp not restored after the panic");
    }
    let (id, handle) = match failed_isolated_instance() {
        Some(found) => found,
        None => fail("isolated-panic-escape: no failed instance recorded"),
    };
    assert_failure_released("isolated-panic-escape", id, handle);
    if !kernel_native_still_works() {
        fail("isolated-panic-escape: KernelNative path broke");
    }
    kernel::log!(
        "selftest",
        "isolated-panic-escape: contained + cleaned (instance={})",
        id.raw()
    );
    pass("isolated-panic-escape")
}

use super::*;
