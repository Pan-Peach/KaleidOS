//! `sched.yield_roundtrip` —— 真实调度交接对的端到端成本（目标端权威测量）。
//!
//! 用 Core 的**既有**设施（无新权限）：`kcore_task_create` / `task_start` 建两个
//! 组件自有任务 A/B，`kcore_sched_run` 进入调度，A 用 `kcore_task_yield` 与 B
//! 交替。时钟只在 A 的 batch 边界读（`measure::batch` 在调用者上下文）。
//!
//! # 测量协议
//!
//! - **body = 一次 A→B→A 往返**：A `yield()` →（Core propose→validate→commit→
//!   context switch）→ B 运行、计数、`yield()` → A 恢复。`ops_per_batch` = 每批
//!   往返数；K 校准、warmup、5×31 有界采样、配对 null baseline 与其它 primitive
//!   完全同一套 `measure` 流程。
//! - **启动/退出在计时之外**：B 的首次激活落在 K=1 warmup（不计入样本）；
//!   A 在全部采样与打印之后才 `task_exit`，B 看到 `A_DONE` 后退出。
//! - **handshake（逐次断言，不是事后抽样）**：每次 yield 前读 B 激活计数、
//!   恢复后必须恰好 +1，否则记 mismatch。任何 mismatch → `status=handshake_mismatch`
//!   （原始数字照打，但明确标注不可信）。`handoff_count` 只统计**正式采样窗口**
//!   （`collect_into` 的 `phase()` 钩子在采样前清零），无 mismatch 时恒等于
//!   `iterations`。
//! - **独立正确性验证（计时之外）**：trace 可用时，A 做 16 次往返，然后按
//!   Core 的 `TaskSwitch` 事件流验证 32 次切换严格 A→B / B→A 交替；
//!   记录被逐出 / 掩码关闭 / pattern 不符 → 如实上报 `trace_validation=`。
//!   这是"用 tracing 验证真实 handoff 数"的独立通道（另一份计数来源）。
//!
//! # 诚实声明（必须连同数字一起读）
//!
//! - 这是**两次调度路径 + 两次 handoff** 的端到端成本（policy 选择 + Core 验证 +
//!   commit + context switch + 测试 harness 的计数/循环），**不是**裸寄存器
//!   save/restore 延迟。
//! - 已有 `TaskSwitch` trace 的 timestamp **不能**测纯切换：它在状态 commit 之后、
//!   真正切走之前记录（见 `sched::schedule_next`），落在路径中间而不是两端。
//! - 数字包含：yield 调用、RR policy 调用、Core 验证、状态提交、`__switch`、
//!   handshake 计数与循环。`handoff_count == iterations` 只证明"每个样本都真的
//!   发生了交替"，不证明切换开销本身。
//! - batch 是在 QEMU TCG 上跑的；只作同环境相对趋势，不做真机预测。

use core::sync::atomic::Ordering;

use kcomp_sdk::abi;
use kcomp_sdk::binding::{self, InterfaceKind};

use crate::{Context, State, report, trace as traceview};

/// 报告块名。
const NAME: &str = "sched.yield_roundtrip";
/// 独立正确性验证块名。
const VERIFY_NAME: &str = "sched.yield_roundtrip.verify";
/// B 连续激活超过 A 已确认 handoff 数 + 本裕量 → 认为 A 卡住，B 退出兜底。
/// 正常交替时 B_ACTIVATIONS == HANDOFFS_TOTAL（差值 0/1）。
const STUCK_SLACK: usize = 8;

// sched 的 per-run 状态已迁入 `crate::State`（任务 id / handoff 计数 / 测量
// context），不再有 image-global static（docs/architecture/component-lifecycle.md §10）。
// 单 CPU 也不构成放开 `&mut` 别名的理由：任务 A/B 与锚点共享同一地址空间，
// 跨任务访问仍走原子字段 / state 指针。

/// 一次 A→B→A 往返：yield 前读 B 的激活计数，恢复后必须恰好 +1。
///
/// 返回 `1`（被 `black_box` 消耗）。handshake 计数本身在计时区间内——它是
/// harness 工作的一部分，报告里如实声明（见模块头）。
fn roundtrip(state: *mut State) -> u64 {
    let before = unsafe { (*state).b_activations.load(Ordering::SeqCst) };
    let rc = unsafe { abi::kcore_task_yield() };
    let after = unsafe { (*state).b_activations.load(Ordering::SeqCst) };
    if rc == 0 && after == before + 1 {
        unsafe {
            (*state).handoffs_total.fetch_add(1, Ordering::SeqCst);
            (*state).handoffs_sampled.fetch_add(1, Ordering::SeqCst);
        }
    } else {
        unsafe { (*state).mismatches.fetch_add(1, Ordering::SeqCst) };
    }
    1
}

/// 任务 A：跑整个测量（warmup → 校准 → 采样 → 打印），然后退出。
///
/// `arg` 是本实例的 `*mut State`（`kcore_task_create` 原样回传）。任务归属仍来自
/// Core 执行边界，**不是**来自 `arg`。
extern "C" fn task_a(arg: *mut ()) {
    let state = arg as *mut State;
    let Some(context) = (unsafe { (*state).sched_context }) else {
        // 不可达：只有 Context 写入后才 start 任务；防御性退出而不是空转。
        let _ = unsafe { abi::kcore_task_exit() };
        loop {
            core::hint::spin_loop();
        }
    };

    // 独立正确性验证（计时之外）：按 Core 的 TaskSwitch 事件流数真实交替。
    let trace_mask = traceview::state().map_or(0, |state| state.enabled_mask);
    let trace_verdict = traceview::validate_task_alternation(
        unsafe { (*state).a_task.load(Ordering::SeqCst) },
        unsafe { (*state).b_task.load(Ordering::SeqCst) },
        traceview::VALIDATION_TRIPS,
        &mut || {
            let _ = roundtrip(state);
        },
    );

    crate::run_primitive_observed(
        state,
        NAME,
        &context,
        || roundtrip(state),
        &mut || {
            unsafe { (*state).handoffs_sampled.store(0, Ordering::SeqCst) };
        },
        &mut || {
            let mismatches = unsafe { (*state).mismatches.load(Ordering::SeqCst) };
            report::key_u64("handoff_count", unsafe {
                (*state).handoffs_sampled.load(Ordering::SeqCst)
            } as u64);
            report::key_u64("handoff_total", unsafe {
                (*state).handoffs_total.load(Ordering::SeqCst)
            } as u64);
            report::key_u64("handoff_mismatches", mismatches as u64);
            report::key_str("trace_validation", trace_verdict);
            // 数字自带的 trace 状态：非 0 = 本块的计时包含 emit 成本
            // （与 BENCH-ENV 同一事实，放在块内防止断章取义）。
            report::key_u64("trace_mask", trace_mask);
            let stuck = unsafe { (*state).stuck.load(Ordering::SeqCst) };
            if mismatches == 0 && trace_verdict != "mismatch" && !stuck {
                "ok"
            } else {
                "handshake_mismatch"
            }
        },
    );

    unsafe { (*state).a_done.store(true, Ordering::SeqCst) };
    let _ = unsafe { abi::kcore_task_exit() };
    loop {
        core::hint::spin_loop();
    }
}

/// 任务 B：应答侧。每次被调度激活就计数并交还 CPU；看到 `A_DONE` 退出。
///
/// 兜底：若连续激活数远超 A 已确认的 handoff（坏策略导致 A 不被恢复），
/// B 主动退出把 CPU 让回 A —— 测量会以 `handshake_mismatch` 结束，而不是挂死。
///
/// `arg` 是本实例的 `*mut State`（与任务 A 同一指针）。
extern "C" fn task_b(arg: *mut ()) {
    let state = arg as *mut State;
    loop {
        if unsafe { (*state).a_done.load(Ordering::SeqCst) } {
            break;
        }
        let activated = unsafe { (*state).b_activations.load(Ordering::SeqCst) };
        if activated
            > unsafe { (*state).handoffs_total.load(Ordering::SeqCst) }.saturating_add(STUCK_SLACK)
        {
            unsafe { (*state).stuck.store(true, Ordering::SeqCst) };
            break;
        }
        unsafe { (*state).b_activations.fetch_add(1, Ordering::SeqCst) };
        let _ = unsafe { abi::kcore_task_yield() };
    }
    let _ = unsafe { abi::kcore_task_exit() };
    loop {
        core::hint::spin_loop();
    }
}

/// `scheduler` Policy 接口是否可用（exact ABI fingerprint）。
fn scheduler_available() -> bool {
    unsafe {
        abi::kcore_interface_available(
            b"scheduler".as_ptr(),
            b"scheduler".len(),
            InterfaceKind::Policy.as_u32(),
            binding::SCHEDULER_POLICY_ABI.raw(),
        ) == 1
    }
}

/// 确认/建立调度配置：优先用已绑定的 scheduler；没有就正常加载参考实现
/// `scheduler_rr`（与 core_test 同一条加载链，无 benchmark 特权）。
fn ensure_scheduler() -> bool {
    if scheduler_available() {
        return true;
    }
    let name = b"scheduler_rr";
    let _ = unsafe { abi::kcore_component_load(name.as_ptr(), name.len()) };
    scheduler_available()
}

fn reset_counters(state: *mut State) {
    unsafe {
        (*state).a_done.store(false, Ordering::SeqCst);
        (*state).stuck.store(false, Ordering::SeqCst);
        (*state).mismatches.store(0, Ordering::SeqCst);
        (*state).handoffs_total.store(0, Ordering::SeqCst);
        (*state).handoffs_sampled.store(0, Ordering::SeqCst);
        (*state).b_activations.store(0, Ordering::SeqCst);
    }
}

/// 终态证据（在锚点上下文、`sched_run` 返回之后）。
fn verify_ok(state: *mut State, run_status: i32, a: i32, b: i32) -> bool {
    let total = unsafe { (*state).handoffs_total.load(Ordering::SeqCst) };
    run_status == 0
        && a == STATE_EXITED
        && b == STATE_EXITED
        && unsafe { (*state).mismatches.load(Ordering::SeqCst) } == 0
        && !unsafe { (*state).stuck.load(Ordering::SeqCst) }
        && unsafe { (*state).b_activations.load(Ordering::SeqCst) } == total
}

/// `kcore_task_state` 的编码：4 = Exited（见 export.rs）。
const STATE_EXITED: i32 = 4;

/// 执行一次 `sched.yield_roundtrip`（从 `kcomp_instance_create` 的锚点上下文调用）。
///
/// `state` 是本实例的 per-run 状态；任务 A/B 经 `kcore_task_create` 的 `arg`
/// 拿到同一指针（opaque，任务归属仍来自 Core 执行边界）。
pub(crate) fn run(state: *mut State, context: &Context) {
    if !ensure_scheduler() {
        report::bench_header(NAME);
        report::key_str("method", "task_handoff");
        report::key_str("status", "scheduler_unavailable");
        return;
    }

    reset_counters(state);
    unsafe { (*state).sched_context = Some(*context) };

    let mut a_id = 0u32;
    let mut b_id = 0u32;
    let create_a = unsafe { abi::kcore_task_create(task_a, state as *mut (), &mut a_id) };
    let create_b = unsafe { abi::kcore_task_create(task_b, state as *mut (), &mut b_id) };
    if create_a != 0 || create_b != 0 || a_id == b_id {
        report::bench_header(NAME);
        report::key_str("method", "task_handoff");
        report::key_i64("task_a_create", i64::from(create_a));
        report::key_i64("task_b_create", i64::from(create_b));
        report::key_str("status", "task_setup_failed");
        return;
    }
    unsafe {
        (*state).a_task.store(a_id, Ordering::SeqCst);
        (*state).b_task.store(b_id, Ordering::SeqCst);
    }

    let started_a = unsafe { abi::kcore_task_start(a_id) };
    let started_b = unsafe { abi::kcore_task_start(b_id) };
    if started_a != 0 || started_b != 0 {
        report::bench_header(NAME);
        report::key_str("method", "task_handoff");
        report::key_i64("task_a_start", i64::from(started_a));
        report::key_i64("task_b_start", i64::from(started_b));
        report::key_str("status", "task_setup_failed");
        return;
    }

    // 锚点进入调度：A 跑完整个测量并退出，B 随后退出，控制权回到这里。
    let run_status = unsafe { abi::kcore_sched_run() };
    let (state_a, state_b) = unsafe { (abi::kcore_task_state(a_id), abi::kcore_task_state(b_id)) };

    report::bench_header(VERIFY_NAME);
    report::key_i64("sched_run_status", i64::from(run_status));
    report::key_u64(
        "handoff_total",
        unsafe { (*state).handoffs_total.load(Ordering::SeqCst) } as u64,
    );
    report::key_u64(
        "b_activations",
        unsafe { (*state).b_activations.load(Ordering::SeqCst) } as u64,
    );
    report::key_u64(
        "handoff_mismatches",
        unsafe { (*state).mismatches.load(Ordering::SeqCst) } as u64,
    );
    report::key_i64("task_a_state", i64::from(state_a));
    report::key_i64("task_b_state", i64::from(state_b));
    report::key_str(
        "stuck",
        if unsafe { (*state).stuck.load(Ordering::SeqCst) } {
            "yes"
        } else {
            "no"
        },
    );
    let ok = verify_ok(state, run_status, state_a, state_b);
    report::key_str("status", if ok { "ok" } else { "mismatch" });
}
