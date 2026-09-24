//! kbench —— 板上 benchmark 组件（真机 / QEMU 的权威测量路径）。
//!
//! 与 host 的 `make bench` 共用同一套输出约定（`BENCH <name>` + `key=value`）
//! 与同一套**批量计时**方法论：
//!
//! - 把 K 次操作夹在**恰好两次读钟**之间，端点量化被摊薄到约 q/K；
//! - 有界时钟探测 → **K=1 先 warmup（冷启动样本不得参与校准）** → 校准 K
//!   （每个 K 跑 3 个 pilot batch 取中位数；K=1 的假 overshoot 不采信）→ 冻结 K
//!   收集 5 轮 × 31 个 batch 样本（有界数组，全部保留）；低于 floor 且有界允许
//!   时翻倍重采（最多 3 次），报告的是修正后的最终 K；
//! - 同一 K 交替测 null baseline（只有循环 + `black_box`），原始 work /
//!   baseline 都报；不减去独立挑选的最小值，不把负差值 clamp 成 0；
//! - **组件内不做除法**：只发原始整数（batch 总 tick）；`ns/op = d * 10^9 /
//!   (K * f)` 由 host 先乘后除。
//!
//! 诚实声明：批量计时修正的是"读钟粒度 + 计时开销占比"，**不能**把 QEMU TCG
//! 变成真实 CPU。min 只是"观测到的最快 batch"，不是精度证明；五轮最小值相同
//! 可能只是落在同一个量化格子里。真实上下文切换 / IRQ 延迟的绝对性能要在
//! 真机测（见 docs/development/benchmark.md）。
//!
//! primitive 分两类：
//! - **无额外 authority**：时钟 / 只读查询（锚点上下文直接跑）；
//! - **真实调度交接**（`sched.yield_roundtrip`）：两个组件自有任务 A/B 走
//!   Core 调度链（创建 / 启动 / yield / exit 全是既有导出，无 benchmark
//!   特权），测量协议与诚实声明见 `sched` 模块头。

#![no_std]

mod irq;
mod measure;
mod report;
mod sched;
mod stats;
mod trace;

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize};

use kcomp_sdk::abi;
use kcomp_sdk::abi::MemoryView;
use kcomp_sdk::errno::Errno;
use kcomp_sdk::mem;

#[cfg(test)]
extern crate std;

/// 测量轮数。
const ROUNDS: usize = 5;
/// 每轮批次数（奇数：中位数是真实观测值）。
const BATCHES_PER_ROUND: usize = 31;
/// 全部 batch 样本（有界、完整保留）。
const SAMPLES: usize = ROUNDS * BATCHES_PER_ROUND;
/// 单批操作数硬上限。
const MAX_OPS_PER_BATCH: u64 = 1 << 20;
/// warmup batch 数（固定，不计入结果；组件内不做除法换算）。
const WARMUP_BATCHES: u64 = 4;
/// 时钟探测的最大读次数（有界：时钟不前进也一定结束）。
const CLOCK_PROBE_READS: u64 = 1024;
/// bracket 开销估计用的空 batch 次数。
const BRACKET_SAMPLES: u64 = 64;

/// 一次运行共享的环境/计划（所有 primitive 用同一份时钟刻画）。
#[derive(Clone, Copy)]
struct Context {
    probe: measure::ClockProbe,
    bracket: u64,
    quantum: u64,
    target: u64,
    cap: u64,
}

/// 一次 benchmark 运行的 per-instance 可变状态（Core backing 分配）。
///
/// 全部从 image-global `static` 迁入这里（`docs/architecture/component-lifecycle.md` §10）：
/// 组件代码常驻，实例状态来自显式分配；create 返回本分配的不透明指针，
/// destroy 释放。state 指针只经 task `arg` / IRQ handler `ctx` 传递，不落回
/// image-global static。
///
/// **组合策略：同一时刻只允许一个活跃 benchmark 运行。** 并发实例会争抢
/// CPU / 时钟 / 调度器 / UART，测量互相污染且没有意义；本组件**不**尝试支持
/// 多实例并发（也不允许多个运行共享同一 state）。
///
/// 采集缓冲（约 2.5 KB）必须在共享堆、不能在任务栈：组件任务内核栈只有 4 KiB
/// （见 `measure::Collected`）。跨界计数器保持原子——单 CPU 也不构成放开
/// `&mut` 别名的理由（trap / 任务上下文可重入）。
pub(crate) struct State {
    pub(crate) collected: measure::Collected,
    // —— `sched.yield_roundtrip` 的 per-run 状态 ——
    pub(crate) a_task: AtomicU32,
    pub(crate) b_task: AtomicU32,
    pub(crate) b_activations: AtomicUsize,
    pub(crate) handoffs_total: AtomicUsize,
    pub(crate) handoffs_sampled: AtomicUsize,
    pub(crate) mismatches: AtomicUsize,
    pub(crate) a_done: AtomicBool,
    pub(crate) stuck: AtomicBool,
    pub(crate) sched_context: Option<Context>,
    // —— `irq.uart_trigger_to_handler` 的 per-run 状态 ——
    pub(crate) irq_entry_low: AtomicU32,
    pub(crate) irq_served: AtomicBool,
    /// claim 返回的 MMIO 寄存器基址（KernelNative 直接访问；0 = 未认领）。
    pub(crate) irq_uart_lease: AtomicUsize,
    /// 认领中的设备身份（仅锚点上下文读写）：正常路径在 `irq::run` 内同步释放并
    /// 清零；destroy 用它兜底 quiesce。
    pub(crate) irq_device: u32,
    /// 本 state 的 backing 窗口（create 的 acquire 交付；destroy 原样交回）。
    pub(crate) region: MemoryView,
}

impl State {
    /// 初值缓冲；create 用 [`core::ptr::write`] 一次写入完成初始化。
    const fn new() -> Self {
        Self {
            collected: measure::Collected::new(),
            a_task: AtomicU32::new(0),
            b_task: AtomicU32::new(0),
            b_activations: AtomicUsize::new(0),
            handoffs_total: AtomicUsize::new(0),
            handoffs_sampled: AtomicUsize::new(0),
            mismatches: AtomicUsize::new(0),
            a_done: AtomicBool::new(false),
            stuck: AtomicBool::new(false),
            sched_context: None,
            irq_entry_low: AtomicU32::new(0),
            irq_served: AtomicBool::new(false),
            irq_uart_lease: AtomicUsize::new(0),
            irq_device: 0,
            region: MemoryView {
                kind: 0,
                reserved: 0,
                base: 0,
                len: 0,
            },
        }
    }
}

/// 借用采集缓冲。单 CPU、协作式调度；primitive 串行执行，同一时刻只有一个
/// 借用者，且不跨 primitive 保存。
fn collected_buffer(state: *mut State) -> &'static mut measure::Collected {
    // SAFETY: state 是本实例的 Core 共享堆分配，生命周期覆盖整个 create；用
    // 裸指针访问避免跨上下文建立长生命周期 `&mut State` 别名。
    unsafe { &mut (*state).collected }
}

/// 校准 K + 采集 + 打印一个 primitive 的完整报告块（无附加观测）。
fn run_primitive<F: FnMut() -> u64>(state: *mut State, name: &str, context: &Context, body: F) {
    run_primitive_observed(state, name, context, body, &mut || {}, &mut || "ok");
}

/// 校准 K + 采集 + 打印一个 primitive 的完整报告块。
///
/// 顺序固定为 **warmup → 校准 → 采集 →（低于 floor 时）有界重采**；报告里的
/// `operations_per_batch` 是修正之后的最终 K。打印在**采样全部结束之后**：
/// 打印本身不能落在任何计时区间里，也不在轮次之间。
///
/// `phase` 在每次 `collect` 的 warmup 之后、正式采样之前调用（sched 用它把
/// 按样本计的 handoff 清零）；`tail` 在统计行之后打印附加 key 并返回 status。
fn run_primitive_observed<F: FnMut() -> u64>(
    state: *mut State,
    name: &str,
    context: &Context,
    mut body: F,
    phase: &mut impl FnMut(),
    tail: &mut impl FnMut() -> &'static str,
) {
    // 0) 先温热（校准之前，K=1）：K=1 的 pilot 若吃到一个冷启动样本（首次
    //    调用 / 首次缺页 / 冷分支），中位数也会被抬高，制造假 overshoot 把 K
    //    钉死在 1；固定 batch 数的 warmup 把被测路径先走热。
    measure::warmup(1, WARMUP_BATCHES, &mut body);

    // 1) 选 K：每个 K 跑 3 个 pilot batch，取中位数（不被一次宿主停顿带偏）。
    let mut ops = measure::choose_ops_per_batch(
        context.target,
        context.cap,
        MAX_OPS_PER_BATCH,
        &mut |candidate| measure::pilot_at(candidate, &mut body),
    );

    // 2) 冻结 K 采集；低于 floor 且 bounds 允许时翻倍重采（有界重试）。
    //    只有重试用尽（或 target > cap）才如实标 resolution_limited。
    let mut baseline = || core::hint::black_box(0u64);
    let mut retries = 0u64;
    let (paired, stats, resolution_limited, round_medians) = loop {
        let collected = collected_buffer(state);
        if !measure::collect_into(
            ops,
            WARMUP_BATCHES,
            &mut body,
            &mut baseline,
            collected,
            phase,
        ) {
            report::bench_header(name);
            report::key_str("status", "clock_unusable");
            return;
        }
        // 配对差值必须在排序之前算（work / baseline 顺序一一对应）；work 排序
        // 只服务统计，原始样本（含 below-floor 值）不被丢弃、不被 clamp。
        let paired = stats::paired_diff_median(&collected.work, &collected.baseline);
        stats::sort(&mut collected.work);
        let stats = stats::summarize(&collected.work, context.target);
        let round_medians = collected.round_medians;
        let next = measure::retry_ops_per_batch(
            ops,
            retries,
            stats.below_floor,
            context.cap,
            MAX_OPS_PER_BATCH,
            |candidate| measure::pilot_at(candidate, &mut body),
        );
        match next {
            Some(next_ops) => {
                ops = next_ops;
                retries += 1;
            }
            None => {
                let resolution_limited = context.target > context.cap || stats.below_floor > 0;
                break (paired, stats, resolution_limited, round_medians);
            }
        }
    };
    let collected = collected_buffer(state);
    stats::sort(&mut collected.baseline);
    let baseline_stats = stats::summarize(&collected.baseline, context.target);

    report::bench_header(name);
    report::key_str("method", "batch");
    report::key_str("unit", "timebase-ticks");
    report::key_str("sample_unit", "batch_total");
    report::key_u64("operations_per_batch", ops);
    report::key_u64("rounds", ROUNDS as u64);
    report::key_u64("batches_per_round", BATCHES_PER_ROUND as u64);
    report::key_u64("iterations", ops.saturating_mul(SAMPLES as u64));
    report::key_u64("samples", SAMPLES as u64);
    report::key_u64("clock_quantum", context.quantum);
    report::key_u64("clock_probe_reads", context.probe.reads);
    report::key_u64("clock_zero_deltas", context.probe.zero_deltas);
    report::key_u64("clock_backwards", context.probe.backwards);
    report::key_u64("clock_observed_min_delta", context.probe.min_positive);
    report::key_u64("clock_median_read_delta", context.probe.median_positive);
    report::key_u64("clock_bracket_min", context.bracket);
    report::key_u64("calibration_target", context.target);
    report::key_u64("batch_cap", context.cap);
    report::key_u64("calibration_retries", retries);
    report::key_u64("min", stats.min);
    report::key_u64("median", stats.median);
    report::key_u64("p95", stats.p95);
    report::key_u64("max", stats.max);
    // mean 不发：组件内不做除法（host 用 total / samples 换算）。
    report::key_u64("total", stats.total);
    let mut round = 0usize;
    while round < ROUNDS {
        report::round_median(round as u64, round_medians[round]);
        round += 1;
    }
    report::key_u64("below_floor_batches", stats.below_floor);
    report::key_str(
        "resolution_limited",
        if resolution_limited { "yes" } else { "no" },
    );
    report::key_str("baseline", "paired_null");
    report::key_u64("baseline_min", baseline_stats.min);
    report::key_u64("baseline_median", baseline_stats.median);
    report::key_u64("baseline_p95", baseline_stats.p95);
    report::key_u64("baseline_max", baseline_stats.max);
    report::key_u64("baseline_total", baseline_stats.total);
    report::key_i64("baseline_paired_diff_median", paired);
    report::key_str("status", tail());
}

// 实例创建入口（C ABI，`docs/architecture/component-lifecycle.md` §4）。
//
// `0` = 成功（`*out_state` = 本实例 state）；负 errno = 失败，Core 走 Failed 且
// **不会**调用 destroy（构造期清理由本入口负责）。
kcomp_sdk::kcomp_instance_create!(|_args, out_state| {
    // per-instance state：从 Core 取一段 backing 并构造，把不透明指针交给 Core
    // （Core 只存/传，不解释布局）。失败返回 -ENOMEM。
    let size = core::mem::size_of::<State>();
    let align = core::mem::align_of::<State>();
    let region = match mem::mem_alloc(size as u64, align as u64) {
        Ok(view) => view,
        Err(_) => return Errno::ENOMEM.code(),
    };
    let state = region.base as *mut State;
    // SAFETY: 刚 acquire、独占、对齐满足 State；一次写入完成初始化。
    unsafe { core::ptr::write(state, State::new()) };
    // SAFETY: state 落在 region 内；destroy 凭同一 view 原样交回。
    unsafe { (*state).region = region };
    // SAFETY: out_state 由 Core 保证可写；Core 只存/传该指针，不解释、不释放。
    unsafe { *out_state = state as *mut () };

    // 组合策略：单例。并发 benchmark 会互相污染 CPU/时钟/调度器/UART 的测量
    // （见 State 文档与 docs/architecture/component-lifecycle.md §10）；不实现多实例并发。

    let timebase_hz = unsafe { abi::kcore_timebase_hz() };
    // 1 ms 的批时长 cap（policy 参数，报告里写出）；换算不用除法：RV32 上
    // `u64 / u64` 会落到 `__udivdi3` libcall（组件禁止）。
    let cap = if timebase_hz == 0 {
        10_000
    } else {
        measure::ticks_per_millisecond(timebase_hz).max(1)
    };
    let probe = measure::probe_clock(CLOCK_PROBE_READS);

    // BENCH-ENV 如实报告 trace 状态：mask != 0 时被测路径可能真的 emit
    // （数字包含记录成本），mask == 0 时只有掩码过滤；`trace_records` 是
    // "确实读到过记录"的证据（编译期 vs 运行时的边界见 trace 模块头）。
    let trace_state = trace::state();
    report::env(
        timebase_hz,
        trace_state.map_or(0, |state| state.enabled_mask),
        trace_state.map_or(0, |state| state.capacity),
        trace::has_records(),
    );

    if !probe.progressed() {
        // 时钟不前进：不做任何测量（绝不硬报数字）。
        report::bench_header("kbench.clock");
        report::key_str("status", "clock_unusable");
        report::write_str("KBENCH DONE\n");
        return 0;
    }

    let bracket = measure::bracket_cost_min(BRACKET_SAMPLES);
    let quantum = 1u64;
    let context = Context {
        probe,
        bracket,
        quantum,
        target: measure::calibration_target(quantum, bracket),
        cap,
    };

    // 1) 时钟读本身（`rdtime` 导出调用的增量成本）。
    run_primitive(state, "kbench.clock_read", &context, || unsafe {
        abi::kcore_now()
    });

    // 2) 只读机器查询：无 authority 成本，纯导出调用开销。
    run_primitive(state, "kbench.free_pages_query", &context, || {
        u64::from(unsafe { abi::kcore_free_page_count() })
    });

    // 3) 真实调度交接对：两个组件自有任务 A/B（既有任务/调度导出，无特权）；
    //    state 经 task `arg` 传入两个任务。
    sched::run(state, &context);

    // 4) 中断：只能用**合法持有的**设备触发（见 irq 模块头）；handler 经
    //    `kcore_irq_register` 的 `ctx` 拿到 state；拿不到 authority 时如实报告
    //    blocked，不制造 benchmark god-mode。
    irq::run(state, &context);

    // 完成标记：runner 必须等到所有 primitive 都打完才 shutdown，否则会匹配到
    // 第一条 `BENCH` 就提前关掉 QEMU，统计行会丢。
    report::write_str("KBENCH DONE\n");

    0
});

// 实例析构入口（C ABI，`docs/architecture/component-lifecycle.md` §4）：Core 停止路径调用。
//
// 本组件在 create 内**同步**建立并释放所有任务 / IRQ / lease（见 `sched` / `irq`
// 模块头）；destroy 先 [`irq::quiesce`] 兜底（释放残留 authority、清 handler 的
// lease 标记），再丢弃 run 状态并释放 per-instance 分配。kbench 不向任何
// consumer 交出 state 指针（无服务 binding），所以释放它是安全的（对比 §8：已
// 暴露给外部 `'static` 引用的 state 才须保留）。
// 失败 → 返回负 errno，Core 置 Failed 且**绝不重试**；成功留一行可观测证据。
kcomp_sdk::kcomp_instance_destroy!(|state| {
    if state.is_null() {
        // 无状态：create 失败 / 未完整构造的实例 Core 不会调 destroy（§3）。
        report::write_str("[kbench] destroy: no state\n");
        return 0;
    }
    let state = state as *mut State;
    // quiesce：清 handler 的 lease 标记并兜底释放可能残留的 IRQ / MMIO authority
    // （正常路径已在 irq 模块内同步释放，故为幂等空操作）。任务 A/B 由 Core 拥有，
    // `kcore_sched_run` 返回时已全部退出，Core 的 Stopping 路径拒绝存活任务；
    // 无强制终止 API（已推迟），因此这里没有可做的任务操作。
    irq::quiesce(state);
    // SAFETY: state 是本组件 create 经 `*out_state` 写回、Core 原样交还的同一分配；
    // region 是 acquire 交付的原样 view。
    let region = unsafe { (*state).region };
    if let Err(error) = mem::mem_release(region) {
        report::write_str("[kbench] destroy: release failed\n");
        return error.code();
    }
    report::write_str("[kbench] destroy: state released\n");
    0
});
