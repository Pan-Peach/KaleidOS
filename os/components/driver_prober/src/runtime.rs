//! 组件运行期实现（引用 `kcore_*`；host `cargo test` 下不编入）。
//!
//! 每实例状态（候选集 + assignment cursor）在 `kcomp_instance_create` 时经
//! `kcore_heap_alloc` 显式分配：指针写入 `*out_state`、作为 `driver.prober` 的
//! ctx 发布，并作为 dispatch 任务的 arg 传入。候选集在 create 写入后只读；
//! cursor 只被 dispatch 与驱动回调（同一协作式执行流）短借改写——状态不再是
//! 可变全局，所有读写都经 ctx / task arg。

use core::cell::UnsafeCell;

use kcomp_sdk::abi;
use kcomp_sdk::binding::{self, DriverProber, DriverProberApi};

use crate::cursor::{AssignmentCursor, ReportError};
use crate::directory::{CANDIDATES, CandidateSet};

/// Core errno 约定（`0` / `-errno`；见 os/core/src/errno.rs）。
const ENOENT: i32 = -2;
const ENOMEM: i32 = -12;
const EFAULT: i32 = -14;
const EINVAL: i32 = -22;

/// 每实例状态：create 分配一次，实例存活期内地址稳定。
///
/// `set` 只在 create 写入（此后只读）；`cursor` 在 dispatch 与驱动回调中改写。
/// 用 `UnsafeCell` 而不是 `&mut Self`：驱动回调会在 dispatch 调用
/// `kcore_component_load` 的嵌套路径里重入本状态（virtio_blk 在自己的 create
/// 里 next_assignment / report_attempt），任何跨 `kcore_*` 调用持有的 `&mut`
/// 都会被这次重入别名掉。
struct ProberState {
    set: CandidateSet,
    cursor: UnsafeCell<AssignmentCursor>,
}

/// Core 原样回传 create 写出的指针（`*out_state` / binding ctx / task arg）；
/// 这里只做类型恢复，不解引用。
fn state_ptr(ctx: *mut ()) -> *mut ProberState {
    ctx as *mut ProberState
}

/// 候选集（create 写入后只读）。
///
/// # Safety
/// `state` 必须是本组件 `kcomp_instance_create` 分配、且实例存活期内的 state。
unsafe fn set<'a>(state: *mut ProberState) -> &'a CandidateSet {
    // SAFETY: 调用者保证 state 有效；create 之后无任何写入。
    unsafe { &(*state).set }
}

/// assignment cursor。
///
/// # Safety
/// 同 [`set`]；且返回的 `&mut` 只允许**短借**——不得跨任何 `kcore_*` 调用持有
/// （dispatch 的 `kcore_component_load` 会在驱动 create 里重入 cursor）。
unsafe fn cursor_mut<'a>(state: *mut ProberState) -> &'a mut AssignmentCursor {
    // SAFETY: 调用者保证 state 有效；单 CPU 协作式执行流内只有一个活跃借用。
    unsafe { &mut *(*state).cursor.get() }
}

/// 构造期失败清理：state 尚未发布给任何 consumer、也没有任务持有它，按契约
/// §3「构造期清理由组件自己负责」归还。已经交给 Core 的存储（task arg）不在此
/// 释放，按 §8 物理驻留。
///
/// # Safety
/// `state` 必须来自本文件里成功的一次 `kcore_heap_alloc`，且未被发布 / 未被
/// 任何任务持有。
unsafe fn free_state(state: *mut ProberState) {
    // SAFETY: 调用者保证指针来自一次成功的 alloc，size / align 完全一致。
    unsafe {
        let _ = abi::kcore_heap_dealloc(
            state.cast::<u8>(),
            core::mem::size_of::<ProberState>(),
            core::mem::align_of::<ProberState>(),
        );
    }
}

fn name(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("<driver>")
}

/// provider 回调：交付该驱动声明的 compatible 命中的下一台候选设备。
///
/// 只读写 prober 自己的 cursor（经 Core 原样回传的 ctx）：不 claim / release
/// 资源，不创建任务。
extern "C" fn next_assignment(
    ctx: *mut (),
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
    let state = state_ptr(ctx);
    // SAFETY: 调用方保证 (ptr, len) 在调用期间有效（C ABI 契约）。
    let driver = unsafe { core::slice::from_raw_parts(driver_name, driver_name_len) };
    // SAFETY: ctx 是 create 发布的本实例 state；短借不跨 kcore_* 调用。
    match unsafe { cursor_mut(state) }.next(driver) {
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
extern "C" fn report_attempt(ctx: *mut (), attempt: u32, outcome: i32, detail: u32) -> i32 {
    let state = state_ptr(ctx);
    // SAFETY: ctx 是 create 发布的本实例 state；短借不跨 kcore_* 调用。
    match unsafe { cursor_mut(state) }.report(attempt, outcome, detail) {
        Ok(()) => 0,
        Err(ReportError::UnknownAttempt) => ENOENT,
        Err(ReportError::NotHanded | ReportError::AlreadyReported) => EINVAL,
    }
}

/// 发布的 function table（`'static`；不可变表按契约 §10 保持共享，Core 只存
/// api 指针）。
static VTABLE: DriverProberApi = DriverProberApi {
    next_assignment,
    report_attempt,
};

/// dispatch 任务体：prober `Ready`、assignment 接口已提交之后运行一次。
///
/// `arg` = create 写入的 state 指针（Core 原样回传）；任务归属仍来自 Core 的
/// 执行边界，**不是** `arg`。对每个候选：枚举其 compatible 命中的**全部**设备
/// （写入 cursor 供驱动细探），只要有一台匹配就 `kcore_component_load` 候选驱动
/// **一次**，然后退出（有限任务，无后台循环）。这里**没有任何 authority 转移**
/// ——驱动在自己的 create 里 claim。
extern "C" fn dispatch_task(arg: *mut ()) {
    let state = state_ptr(arg);
    // SAFETY: arg 是 create 传入的本实例 state；set 之后只读。
    let candidates = unsafe { set(state) };
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
                // SAFETY: 短借 cursor，不跨下面的 kcore_component_load 持有。
                if !unsafe { cursor_mut(state) }.push(driver, device_id) {
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
        // SAFETY: driver 是本帧有效的 'static 字节串；嵌套的驱动 create 会经
        // ctx 重入 cursor，但此处不持有 cursor 借用。
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

// ---------------------------------------------------------------------------
// 生命周期入口（契约 §4）
// ---------------------------------------------------------------------------

// create 入口：分配并构造每实例 state → staged publish（ctx = state）→
// 有限 dispatch 任务（arg = state）→ 写回 `*out_state`。
kcomp_sdk::kcomp_instance_create!(|_args, out_state| {
    // 1) 每实例 state：去重候选集 + assignment cursor（不再有可变全局）。
    let state = unsafe {
        abi::kcore_heap_alloc(
            core::mem::size_of::<ProberState>(),
            core::mem::align_of::<ProberState>(),
        )
    };
    if state.is_null() {
        kcomp_sdk::klog!("driver_prober: state allocation failed");
        return ENOMEM;
    }
    let state = state.cast::<ProberState>();
    // SAFETY: 刚分配、无别名；ptr::write 直接放置初始值（不读旧值）。
    unsafe {
        core::ptr::write(
            core::ptr::addr_of_mut!((*state).set),
            CandidateSet::build(CANDIDATES),
        );
        core::ptr::write((*state).cursor.get(), AssignmentCursor::new());
    }

    // SAFETY: 刚写入的本实例 state。
    if unsafe { set(state) }.is_empty() {
        kcomp_sdk::klog!("driver_prober: empty candidate directory");
        unsafe { free_state(state) };
        return ENOENT;
    }

    // 2) staged publish assignment Service（Core 在 create 返回 0 后原子提交）；
    //    ctx = 本实例 state：驱动回调经它读写 cursor。
    // SAFETY: VTABLE 是 'static 不可变表；state 在本实例存活期内地址稳定。
    let published = unsafe {
        binding::publish_named::<DriverProber>(
            binding::DRIVER_PROBER_NAME,
            &VTABLE,
            state as *mut (),
        )
    };
    if let Err(status) = published {
        kcomp_sdk::klog!("driver_prober: publish driver.prober failed");
        unsafe { free_state(state) };
        return status;
    }

    // 3) 有限 dispatch 任务：create 返回、接口 commit、prober Ready 之后才运行；
    //    state 经 task arg 传入（任务归属仍来自 Core 执行边界，不是 arg）。
    let mut task = 0u32;
    // SAFETY: entry 在本镜像内；arg = 本实例 state；out_task 可写。
    let rc = unsafe { abi::kcore_task_create(dispatch_task, state as *mut (), &mut task) };
    if rc < 0 {
        kcomp_sdk::klog!("driver_prober: dispatch task create failed (rc={})", rc);
        unsafe { free_state(state) };
        return rc;
    }
    // SAFETY: task 由上一行成功创建。
    let started = unsafe { abi::kcore_task_start(task) };
    if started != 0 {
        kcomp_sdk::klog!("driver_prober: dispatch task start failed (rc={})", started);
        // 任务记录已持有 `arg` = state：实例将走 Failed（`may_run` 门禁保证
        // 该任务不会被调度），但 state 已是 Core 持有的存储，按契约 §8 物理
        // 驻留、不在此释放。
        return started;
    }

    // 成功：state 交给 Core（binding ctx 与 task arg 均已指向它）。
    // SAFETY: out_state 由 Core 提供、可写；state 已完整构造。
    unsafe { core::ptr::write(out_state, state as *mut ()) };

    kcomp_sdk::klog!(
        "driver_prober: {} candidate(s); dispatch queued",
        unsafe { set(state) }.len()
    );
    0
});

// 析构钩子：Core 停止路径（monitor `unload`）在实例无存活任务后调用（dispatch
// 是有限任务，自行 kcore_task_exit；本组件不持有 authority，无需 release）。
// state 存储按契约 §8 **保留**：它已是 `driver.prober` 的 ctx，可能被 consumer
// 拷贝过的 binding 引用，释放会把 stale 逻辑访问变成 use-after-free。
kcomp_sdk::kcomp_instance_destroy!(|_state| {
    kcomp_sdk::klog!("driver_prober: destroy");
    0
});
