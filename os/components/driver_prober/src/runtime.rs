//! 组件运行期实现（引用 `kcore_*`；host `cargo test` 下不编入）。
//!
//! # 无环分发（step 4）
//!
//! 旧流程里驱动在**自己的 create 中回调 prober**（assignment 回调 Service），形成
//! `Task(prober) → Driver create → Service(prober)` 同步重入环。现在流程改为
//! **数据进 create、结果走出来**，prober 组件不再发布任何 endpoint：
//!
//! ```text
//! dispatch 任务（Task(prober)）
//!   → 枚举候选 compatible 命中的全部设备，写进 prober-owned cursor
//!   → 对每个未下发的 assignment：
//!       构造 DriverCreateConfig { device_id, 结果端口名 }（扁平字节）
//!       → kcore_component_create(driver, config)
//!       → create 返回 0 后 lookup + pull `probe.result`（经 Core call gate）
//!       → cursor.report(attempt, outcome, detail)   ← 普通本地调用
//!   → Match 即停止（首个成功 block attach；cursor 更新是本地函数调用）
//! ```
//!
//! **驱动绝不回调 prober**：它只读 create config、claim `DeviceId`、发布自己的
//! 结果端口。重入环因此不存在——不是靠放开 re-entry 门禁，而是把流程变无环。
//!
//! # 每实例状态
//!
//! 候选集 + assignment cursor 仍经 `kcore_heap_alloc` 在 create 显式分配，作为
//! task arg 传入。与旧实现不同，本组件不发布 endpoint、不暴露 ctx：cursor 只被
//! dispatch 任务读写，因此不需要 `UnsafeCell`，跨 `kcore_*` 调用持有 `&mut` 也
//! 不会别名（唯一引用者就是本任务）。
//!
//! `attempt` 映射到结果端口名 `probe.result.<attempt>`（实例内唯一、与保留名
//! `probe.result` 不同）；每次 create 只带**一台**设备，因此一个候选设备对应一个
//! driver 实例（NoMatch 的实例是无资源、无 block endpoint 的 report-only 实例）。

use kcomp_sdk::abi;
use kcomp_sdk::endpoint::Endpoint;
use kcomp_sdk::errno::Errno;
use kcomp_sdk::probe::{self, DriverCreateConfig, ProbeReply, ProbeResult};

use crate::cursor::AssignmentCursor;
use crate::directory::{CANDIDATES, CandidateSet};

/// 每实例状态：create 分配一次，实例存活期内地址稳定。
struct ProberState {
    set: CandidateSet,
    cursor: AssignmentCursor,
}

/// Core 原样回传 create 写出的指针（task arg）；这里只做类型恢复，不解引用。
fn state_ptr(ctx: *mut ()) -> *mut ProberState {
    ctx as *mut ProberState
}

/// 构造期失败清理：state 尚未发布给任何 consumer、也没有任务持有它，按契约
/// §3「构造期清理由组件自己负责」归还。已经交给 Core 的存储（task arg）不在此
/// 释放，按 §8 物理驻留。
///
/// # Safety
/// `state` 必须来自本文件里成功的一次 `kcore_heap_alloc`，且未被任何任务持有。
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

/// 本地记录一个 attempt 的结果（**不是** endpoint 回调）：cursor 只被本任务改写，
/// 记录失败只会是内部 bug（stale / 重复），记日志即可。
fn record(state: &mut ProberState, attempt: u32, reply: ProbeReply) {
    if let Err(error) = state.cursor.report(attempt, reply.outcome, reply.detail) {
        kcomp_sdk::klog!(
            "driver_prober: report attempt={} rejected ({:?})",
            attempt,
            error
        );
    }
}

/// 把某候选声明的 compatible 命中的全部设备枚举进 cursor。
fn enumerate(set: &CandidateSet, cursor: &mut AssignmentCursor, index: usize) {
    let driver = set.driver(index);
    for &compatible in set.compatibles(index) {
        let mut ordinal = 0u32;
        loop {
            let mut device_id = 0u32;
            // SAFETY: compatible 是 'static 字节串；out 可写。
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
            if !cursor.push(driver, device_id) {
                kcomp_sdk::klog!(
                    "driver_prober: assignment cursor full; {} truncated",
                    name(driver)
                );
                return;
            }
            ordinal += 1;
        }
    }
}

/// dispatch 任务体：prober `Ready` 之后运行一次（有限任务）。
///
/// `arg` = create 写入的 state 指针（Core 原样回传）；任务归属仍来自 Core 的
/// 执行边界，**不是** `arg`。`kcore_component_create` 里的 driver create 在
/// 自己的身份下 claim；本任务只读结果、只写自己的 cursor。
extern "C" fn dispatch_task(arg: *mut ()) {
    // SAFETY: arg 是 create 写入的本实例 state；本任务独占它（组件不发布
    // endpoint、不暴露 ctx），地址在实例存活期内稳定。
    let state = unsafe { &mut *state_ptr(arg) };
    let mut stopped = false;

    for index in 0..state.set.len() {
        let driver = state.set.driver(index);
        kcomp_sdk::klog!("driver_prober: loading candidate {}", name(driver));
        enumerate(&state.set, &mut state.cursor, index);

        // 本地取下一台未下发设备（cursor 只被本任务读写）。
        while let Some((attempt, device_id)) = state.cursor.next(driver) {
            let mut name_buf = [0u8; probe::RESULT_PORT_NAME_MAX];
            let name_len = match probe::result_port_name(attempt, &mut name_buf) {
                Ok(len) => len,
                Err(_) => {
                    // u32 attempt 不会失败；保留记录路径而不是 panic。
                    record(state, attempt, ProbeReply::creation_failed(Errno::EINVAL));
                    continue;
                }
            };
            let endpoint_name = &name_buf[..name_len];
            let mut config_buf = [0u8; DriverCreateConfig::MAX_ENCODED_LEN];
            let config_len =
                match DriverCreateConfig::new(device_id, endpoint_name).encode(&mut config_buf) {
                    Ok(len) => len,
                    Err(_) => {
                        record(state, attempt, ProbeReply::creation_failed(Errno::EINVAL));
                        continue;
                    }
                };
            let args = abi::KcompCreateArgs {
                config_abi: probe::KCOMP_DRIVER_CREATE_CONFIG_ABI,
                config: config_buf.as_ptr().cast(),
                config_len,
            };

            // assignment 经 create config 进入 driver：它读 config、在自己的
            // create 身份下 claim——这里**没有**回调进 prober 的路径。
            kcomp_sdk::klog!(
                "driver_prober: create {} attempt={} device_id={}",
                name(driver),
                attempt,
                device_id
            );
            let mut instance = 0u32;
            // SAFETY: driver / args 均在本帧有效；create 只在调用期间借用 config。
            let status = unsafe {
                abi::kcore_component_create(driver.as_ptr(), driver.len(), &args, &mut instance)
            };
            if status != 0 {
                // construction failure：与 NoMatch 分开记录（outcome = create errno）。
                kcomp_sdk::klog!(
                    "driver_prober: create {} attempt={} failed rc={}",
                    name(driver),
                    attempt,
                    status
                );
                record(state, attempt, ProbeReply::new(status, 0));
                continue;
            }
            kcomp_sdk::klog!(
                "driver_prober: created {} instance={} attempt={}",
                name(driver),
                instance,
                attempt
            );

            // create 返回 0 之后 pull 结果：endpoint 已原子提交，拉取只读。
            let reply = match Endpoint::<ProbeResult>::lookup(instance, endpoint_name) {
                Ok(endpoint) => {
                    kcomp_sdk::klog!(
                        "driver_prober: pull probe.result.{} endpoint={}",
                        attempt,
                        endpoint.id()
                    );
                    match probe::pull_result(endpoint) {
                        Ok(reply) => reply,
                        Err(error) => {
                            kcomp_sdk::klog!(
                                "driver_prober: pull attempt={} failed: {:?}",
                                attempt,
                                error
                            );
                            ProbeReply::creation_failed(Errno::EIO)
                        }
                    }
                }
                Err(error) => {
                    kcomp_sdk::klog!(
                        "driver_prober: result endpoint lookup attempt={} failed: {:?}",
                        attempt,
                        error
                    );
                    ProbeReply::creation_failed(Errno::EIO)
                }
            };
            kcomp_sdk::klog!(
                "driver_prober: attempt={} outcome={} ({}) detail={}",
                attempt,
                reply.outcome,
                reply.outcome_name(),
                reply.detail
            );
            record(state, attempt, reply);
            if reply.is_match() {
                kcomp_sdk::klog!(
                    "driver_prober: attempt={} Match; stopping after first attachment",
                    attempt
                );
                stopped = true;
                break;
            }
        }
        if stopped {
            break;
        }
    }
    if !stopped {
        kcomp_sdk::klog!("driver_prober: no supported device; dispatch done");
    }

    let _ = unsafe { abi::kcore_task_exit() };
    loop {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// 生命周期入口（契约 §4）
// ---------------------------------------------------------------------------

// create 入口：分配并构造每实例 state → 有限 dispatch 任务（arg = state）→
// 写回 `*out_state`。**不发布 endpoint**：driver 不再回调 prober。
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
        return Errno::ENOMEM.code();
    }
    let state = state.cast::<ProberState>();
    // SAFETY: 刚分配、无别名；ptr::write 直接放置初始值（不读旧值）。
    unsafe {
        core::ptr::write(
            core::ptr::addr_of_mut!((*state).set),
            CandidateSet::build(CANDIDATES),
        );
        core::ptr::write(
            core::ptr::addr_of_mut!((*state).cursor),
            AssignmentCursor::new(),
        );
    }

    // SAFETY: 刚写入的本实例 state。
    if unsafe { &(*state).set }.is_empty() {
        kcomp_sdk::klog!("driver_prober: empty candidate directory");
        unsafe { free_state(state) };
        return Errno::ENOENT.code();
    }

    // 2) 有限 dispatch 任务：create 返回、prober Ready 之后才运行；state 经
    //    task arg 传入（任务归属仍来自 Core 执行边界，不是 arg）。
    let mut task = 0u32;
    // SAFETY: entry 在本镜像内；arg = 本实例 state；out_task 可写。
    let rc = unsafe { abi::kcore_task_create(dispatch_task, state.cast::<()>(), &mut task) };
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

    // 成功：state 交给 Core（仅 task arg 指向它）。
    // SAFETY: out_state 由 Core 提供、可写；state 已完整构造。
    unsafe { core::ptr::write(out_state, state.cast::<()>()) };

    kcomp_sdk::klog!(
        "driver_prober: {} candidate(s); dispatch queued",
        unsafe { &(*state).set }.len()
    );
    0
});

// 析构钩子：Core 停止路径（monitor `unload`）在实例无存活任务后调用（dispatch
// 是有限任务，自行 `kcore_task_exit`；本组件不持有 authority，无需 release）。
// state 存储按契约 §8 **保留**：任务记录仍持有 `arg`，释放会让陈旧逻辑访问
// 变成 use-after-free。
kcomp_sdk::kcomp_instance_destroy!(|_state| {
    kcomp_sdk::klog!("driver_prober: destroy");
    0
});
