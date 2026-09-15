//! 组件运行期实现（引用 `kcore_*`；host `cargo test` 下不编入）。
//!
//! 两个静态真相点分开存放，避免同一 `UnsafeCell` 上“读候选集 + 改 cursor”的别名：
//! 候选集在 `kcomp_init` 内、创建 dispatch 任务之前写入，之后只读；cursor 只被
//! dispatch 与驱动回调（同一协作式执行流）触碰。

use core::cell::UnsafeCell;

use kcomp_sdk::abi;
use kcomp_sdk::binding::{self, DriverProber, DriverProberApi};

use crate::cursor::{AssignmentCursor, ReportError};
use crate::directory::{CANDIDATES, CandidateSet};

/// Core errno 约定（`0` / `-errno`；见 os/core/src/errno.rs）。
const ENOENT: i32 = -2;
const EFAULT: i32 = -14;
const EINVAL: i32 = -22;

struct SetCell(UnsafeCell<CandidateSet>);
// SAFETY: phase 1 单 CPU 协作式调度；init 写入后只读，无并发。
unsafe impl Sync for SetCell {}

struct CursorCell(UnsafeCell<AssignmentCursor>);
// SAFETY: phase 1 单 CPU 协作式调度；只有 dispatch 与驱动回调（同一执行流）改写。
unsafe impl Sync for CursorCell {}

static SET: SetCell = SetCell(UnsafeCell::new(CandidateSet::new()));
static CURSOR: CursorCell = CursorCell(UnsafeCell::new(AssignmentCursor::new()));

fn set() -> &'static CandidateSet {
    // SAFETY: 'static；init 之后不再有可变借用。
    unsafe { &*SET.0.get() }
}

fn set_mut() -> &'static mut CandidateSet {
    // SAFETY: 调用者保证是 init 阶段、尚未创建 dispatch 任务、无并发。
    unsafe { &mut *SET.0.get() }
}

fn cursor_mut() -> &'static mut AssignmentCursor {
    // SAFETY: 调用者保证处于单任务协作式执行流（无并发）。
    unsafe { &mut *CURSOR.0.get() }
}

fn name(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("<driver>")
}

/// provider 回调：交付该驱动声明的 compatible 命中的下一台候选设备。
///
/// 只读写 prober 自己的 cursor：不 claim / release 资源，不创建任务。
extern "C" fn next_assignment(
    _ctx: *mut (),
    driver_name: *const u8,
    driver_name_len: usize,
    out_attempt: *mut u32,
    out_device_id: *mut u32,
) -> i32 {
    if out_attempt.is_null() || out_device_id.is_null() {
        return EFAULT;
    }
    if driver_name.is_null() {
        return EINVAL;
    }
    // SAFETY: 调用方保证 (ptr, len) 在调用期间有效（C ABI 契约）。
    let driver = unsafe { core::slice::from_raw_parts(driver_name, driver_name_len) };
    match cursor_mut().next(driver) {
        Some((attempt, device_id)) => {
            // SAFETY: out 可写由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
            unsafe {
                core::ptr::write_unaligned(out_attempt, attempt);
                core::ptr::write_unaligned(out_device_id, device_id);
            }
            0
        }
        None => ENOENT,
    }
}

/// provider 回调：记录驱动对某个 `attempt` 的结果（只更新 prober 记录）。
extern "C" fn report_attempt(_ctx: *mut (), attempt: u32, outcome: i32, detail: u32) -> i32 {
    match cursor_mut().report(attempt, outcome, detail) {
        Ok(()) => 0,
        Err(ReportError::UnknownAttempt) => ENOENT,
        Err(ReportError::NotHanded | ReportError::AlreadyReported) => EINVAL,
    }
}

/// 发布的 function table（`'static`；Core 只存 api 指针）。
static VTABLE: DriverProberApi = DriverProberApi {
    next_assignment,
    report_attempt,
};

/// dispatch 任务体：prober `Ready`、assignment 接口已提交之后运行一次。
///
/// 对每个候选：枚举其 compatible 命中的**全部**设备（写入 cursor 供驱动细探），
/// 只要有一台匹配就 `kcore_component_load` 候选驱动**一次**，然后退出（有限任务，
/// 无后台循环）。这里**没有任何 authority 转移**——驱动在自己的 init 里 claim。
extern "C" fn dispatch_task() -> ! {
    let candidates = set();
    for index in 0..candidates.len() {
        let driver = candidates.driver(index);
        let mut matched = false;
        for &compatible in candidates.compatibles(index) {
            let mut ordinal = 0u32;
            loop {
                let mut device_id = 0u32;
                // SAFETY: compatible 是本帧有效的 'static 字节串；out 可写。
                let rc = unsafe {
                    abi::kcore_device_nth(
                        compatible.as_ptr(),
                        compatible.len(),
                        ordinal,
                        &mut device_id,
                    )
                };
                if rc != 0 {
                    break; // `-ENOENT` = 已枚举完（唯一终止信号）。
                }
                if !cursor_mut().push(driver, device_id) {
                    kcomp_sdk::klog!(
                        "driver_prober: assignment cursor full; {} truncated",
                        name(driver)
                    );
                    break;
                }
                matched = true;
                ordinal += 1;
            }
        }
        if !matched {
            kcomp_sdk::klog!("driver_prober: no device for {}; skip", name(driver));
            continue;
        }
        kcomp_sdk::klog!("driver_prober: loading candidate {}", name(driver));
        // SAFETY: driver 是本帧有效的 'static 字节串。
        let rc = unsafe { abi::kcore_component_load(driver.as_ptr(), driver.len()) };
        if rc >= 0 {
            kcomp_sdk::klog!("driver_prober: {} loaded (id={})", name(driver), rc);
        } else {
            kcomp_sdk::klog!("driver_prober: load {} failed (rc={})", name(driver), rc);
        }
    }
    let _ = unsafe { abi::kcore_task_exit() };
    loop {
        core::hint::spin_loop();
    }
}

kcomp_sdk::kcomp_init!({
    // 1) 从静态目录构建去重候选集（compatible 只是 opaque key，不解释）。
    *set_mut() = CandidateSet::build(CANDIDATES);
    if set().is_empty() {
        kcomp_sdk::klog!("driver_prober: empty candidate directory");
        return -1;
    }

    // 2) staged publish assignment Service（Core 在 init 返回 0 后原子提交）。
    let published =
        unsafe { binding::publish_service::<DriverProber>(&VTABLE, core::ptr::null_mut()) };
    if published.is_err() {
        kcomp_sdk::klog!("driver_prober: publish driver.prober failed");
        return -1;
    }

    // 3) 有限 dispatch 任务：init 返回、接口 commit、prober Ready 之后才运行。
    let task = unsafe { abi::kcore_task_create(dispatch_task as *const () as usize) };
    if task < 0 {
        kcomp_sdk::klog!("driver_prober: dispatch task create failed (rc={})", task);
        return task;
    }
    let started = unsafe { abi::kcore_task_start(task as u32) };
    if started != 0 {
        kcomp_sdk::klog!("driver_prober: dispatch task start failed (rc={})", started);
        return started;
    }

    kcomp_sdk::klog!(
        "driver_prober: {} candidate(s); dispatch queued",
        set().len()
    );
    0
});

// 退出钩子：Core 停止路径（monitor `unload`）会调用。driver_prober 不持有
// authority，显式 no-op；注意 dispatch 任务未退出时 stop 会拒绝（drain 未实现，
// 见 docs/component-model.md §5.2）。
kcomp_sdk::kcomp_exit!(0);
