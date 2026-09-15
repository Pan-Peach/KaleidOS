//! KaleidOS benchmark harness —— 统一的测量与报告，**不与 correctness test 混用**。
//!
//! 方法论参考 lmbench / UnixBench 的"拆 primitive"思路（**不照搬**它们的 POSIX
//! 测例）：一个 benchmark 只测一个 primitive，只报告原始数据，不构造"总分"。
//!
//! # 批量计时（batch timing）
//!
//! 单次操作夹在两次读钟之间会得到 `d = op + 端点量化 + 读钟开销`。在 10 MHz
//! timebase（1 tick = 100 ns）上，一次短操作的 `d` 可能只有个位数 tick ——
//! 量化误差占比极大，而且**可能向下取整**，所以"取最小"并不能消除误差。
//!
//! 本 harness 改为：把 `K` 次操作夹在**恰好两次读钟**之间，
//! `d = t_after - t_before`（结果消耗 `black_box` 也在计时区间内）。
//! 端点量化被摊薄到约 `q/K`（q 为一个 tick）。这**不**消除宿主抖动 / 循环
//! 开销 / bias —— 它只修正"读钟粒度 + 计时开销占比"这一项。
//!
//! 一次 primitive 的流程：
//! 1. **时钟刻画**：有界探测（零 delta、最小正 delta、典型读钟 delta）
//!    与空 batch 的 bracket 开销；
//! 2. **先温热**（在校准之前，K=1、固定 batch 数）：冷启动样本若进了 pilot，
//!    K=1 会假 overshoot，把 K 钉死在 1（测量全部低于 floor）；
//! 3. **选 K**：从 1 翻倍，每个 K 跑 3 个 pilot batch 取**中位数**，直到 batch
//!    时长达到 `max(200*q, 100*bracket)`，或触及 1 ms cap / K 上限；
//! 4. **冻结 K 采集**：warmup 后收集 `ROUNDS × BATCHES_PER_ROUND` 个 batch 样本
//!    （有界数组，全部保留，不截断）；若本轮低于 floor 且 bounds 允许（未到
//!    [`MAX_RETRIES`]、`2K` 不超 `MAX_OPS_PER_BATCH` 与 cap），翻倍 K 重采，
//!    报告只写最终 K；只有重试用尽（或 `target > cap`）才标 resolution-limited；
//! 5. **配对 baseline**：同一 K 交替测 null baseline（只有循环 / `black_box`，
//!    没有实际工作），原始 work 与 baseline 都报；不减去独立挑选的最小值，
//!    不把负差值 clamp 成 0；
//! 6. **报告**：统计对象是 **batch 总时长**（分母 `operations_per_batch`），
//!    换算成 ns/op 由 host 工具做（先乘后除，测量端不截断）。
//!
//! 必须避免（否则数字没有意义）：
//! - 编译器把被测体优化掉 —— 用 [`core::hint::black_box`] 兜住返回值；
//! - 测量循环里打印 —— 本模块只在结束时报告一次；
//! - 首次初始化污染 steady-state —— 校准前先做固定批数的 K=1 warmup，
//!   调用方也可再给 `warmup`（不计入结果）；
//! - 关中断"制造干净数字" —— 本模块不做，也不允许调用方借它做。
//!
//! 时钟统一走 [`now`]：目标端是 `rdtime`（timebase tick），host 测试是
//! `std::time::Instant`（ns）。单位不同，所以报告必须同时给出单位与来源
//! （[`report_environment`]），否则跨平台数字不可比。
//!
//! 本模块**不**负责 platform 判定：Core 没有运行时的板级/QEMU 探测，QEMU 与
//! 真机的区分必须由 runner 记录（`report_environment` 里如实写 `undetected`）。

use alloc::vec::Vec;

mod batch;
mod irq;
mod report;

pub use batch::{
    BRACKET_SAMPLES, BatchStats, CLOCK_PROBE_READS, Calibration, ClockProbe, MAX_RETRIES,
    MeasurementPlan, batch_cap, bracket_cost_min, calibration_target, choose_ops_per_batch,
    measurement_plan, probe_clock, resolution_limited, retry_ops_per_batch, summarize,
};
pub use irq::{IrqLatency, collect_irq_latency, irq_latency};
pub use report::{BenchResult, BenchStatus, report_environment};

/// 时钟单位 —— 报告里必须写清楚。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockUnit {
    /// `rdtime` 的 timebase tick：换算成时间需要 `MachineInfo.timebase_frequency`。
    TimebaseTicks,
    /// 纳秒。
    Nanoseconds,
}

impl ClockUnit {
    pub const fn as_str(self) -> &'static str {
        match self {
            ClockUnit::TimebaseTicks => "timebase-ticks",
            ClockUnit::Nanoseconds => "ns",
        }
    }
}

#[cfg(test)]
mod clock {
    use super::ClockUnit;
    use std::sync::OnceLock;
    use std::time::Instant;

    /// 进程内 epoch：`Instant` 没有绝对零点，用第一次调用建一个。
    static EPOCH: OnceLock<Instant> = OnceLock::new();

    pub fn now() -> u64 {
        let epoch = EPOCH.get_or_init(Instant::now);
        epoch.elapsed().as_nanos() as u64
    }

    pub const fn unit() -> ClockUnit {
        ClockUnit::Nanoseconds
    }

    pub const fn source() -> &'static str {
        "std::time::Instant"
    }
}

#[cfg(not(test))]
mod clock {
    use super::ClockUnit;
    use arch::{Timer, TimerImpl};

    pub fn now() -> u64 {
        TimerImpl::now()
    }

    pub const fn unit() -> ClockUnit {
        ClockUnit::TimebaseTicks
    }

    pub const fn source() -> &'static str {
        "rdtime (arch::TimerImpl)"
    }
}

pub use clock::{now, source as clock_source, unit as clock_unit};

/// 测量轮数。每轮单独算中位数；轮间散布就是观测到的干扰量级。
pub const ROUNDS: usize = 5;

/// 每轮批次数。31 是奇数：中位数是真实观测值，不是两个样本的平均。
pub const BATCHES_PER_ROUND: usize = 31;

/// batch 样本总量 = `ROUNDS × BATCHES_PER_ROUND`，**全部保留**。
///
/// 这里没有"只统计前 1024 个样本"的截断：样本总量由这个常数限定，
/// min / median / p95 / max 覆盖**整个**测量。
pub const SAMPLE_CAP: usize = ROUNDS * BATCHES_PER_ROUND;

/// 单批操作数硬上限：防止 K 在极快操作上失控增长。
pub const MAX_OPS_PER_BATCH: u64 = 1 << 20;

/// warmup 的 batch 数上限（warmup 不计入结果，只防病态参数）。
const MAX_WARMUP_BATCHES: u64 = 1024;

/// 校准前固定 warmup 的 batch 数（K=1）。
///
/// 冷启动只发生在前几个调用；固定（而不是调用方给的 `warmup` 换算）才能保证
/// 每个 primitive 都先把冷路径走热，再让 pilot 决定 K。组件端同值。
const PRE_CALIBRATION_WARMUP_BATCHES: u64 = 4;

/// 采集器：校准 K 后收集有界的 work / baseline batch 样本。
pub struct Bench {
    name: &'static str,
    status: BenchStatus,
    plan: MeasurementPlan,
    samples: Vec<u64>,
    baseline_samples: Vec<u64>,
    round_medians: [u64; ROUNDS],
    /// 低于 floor 后的翻倍重采次数（有界 [`MAX_RETRIES`]，随结果一起报出）。
    calibration_retries: u64,
}

impl Bench {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            status: BenchStatus::Ok,
            plan: MeasurementPlan::unmeasured(),
            samples: Vec::with_capacity(SAMPLE_CAP),
            baseline_samples: Vec::with_capacity(SAMPLE_CAP),
            round_medians: [0; ROUNDS],
            calibration_retries: 0,
        }
    }

    /// 校准 + 采集。K 由校准策略决定（目标批时长 / 1 ms cap / K 上限），
    /// **不**由调用方指定：调用方只给 `warmup`（操作数，按冻结的 K 成批执行，
    /// 不计入结果）。
    ///
    /// 顺序固定为 **warmup → 校准 → 采集 →（低于 floor 时）有界重采**；报告里的
    /// `operations_per_batch` 是修正之后的最终 K。
    pub fn run<R>(&mut self, warmup: u64, mut body: impl FnMut() -> R) {
        let probe = probe_clock(CLOCK_PROBE_READS);
        let bracket = if probe.progressed() {
            bracket_cost_min(BRACKET_SAMPLES)
        } else {
            0
        };
        let cap = batch_cap();

        // 先温热（校准之前）：K=1 的 pilot 若吃到一个冷启动样本（首次调用 /
        // 首次缺页 / 冷分支），中位数也会被抬高，制造假 overshoot 把 K 钉死在 1。
        if probe.progressed() {
            for _ in 0..PRE_CALIBRATION_WARMUP_BATCHES {
                let _ = measure_batch(1, &mut body);
            }
        }

        let plan = {
            let mut pilot = |ops: u64| {
                let a = measure_batch(ops, &mut body).unwrap_or(u64::MAX);
                let b = measure_batch(ops, &mut body).unwrap_or(u64::MAX);
                let c = measure_batch(ops, &mut body).unwrap_or(u64::MAX);
                batch::median3(a, b, c)
            };
            measurement_plan(probe, bracket, cap, MAX_OPS_PER_BATCH, &mut pilot)
        };
        self.plan = plan;
        if !plan.usable {
            self.fail_unusable();
            return;
        }

        // 冻结 K 采集；低于 floor 且 bounds 允许时翻倍重采（最多 MAX_RETRIES 次）。
        // 重试用新的 pilot 校验 2K 不超过 cap；最终 K 写回 plan 快照。
        let mut ops = plan.calibration.ops_per_batch;
        let mut retries = 0u64;
        loop {
            if !self.collect_at(ops, warmup, &mut body) {
                self.fail_unusable();
                return;
            }
            self.samples.sort_unstable();
            let stats = summarize(&self.samples, plan.target);
            let next = {
                let mut pilot = |candidate: u64| {
                    let a = measure_batch(candidate, &mut body).unwrap_or(u64::MAX);
                    let b = measure_batch(candidate, &mut body).unwrap_or(u64::MAX);
                    let c = measure_batch(candidate, &mut body).unwrap_or(u64::MAX);
                    batch::median3(a, b, c)
                };
                retry_ops_per_batch(
                    ops,
                    retries,
                    stats.below_floor,
                    cap,
                    MAX_OPS_PER_BATCH,
                    &mut pilot,
                )
            };
            match next {
                Some(next_ops) => {
                    ops = next_ops;
                    retries += 1;
                }
                None => break,
            }
        }
        self.plan.calibration.ops_per_batch = ops;
        self.calibration_retries = retries;
    }

    /// 以冻结的 `ops` 采集一轮：warmup（按 `ops` 成批，不计入）→ 5 轮 × 31 batch，
    /// 每轮交替 work / null baseline（消耗结果的黑盒也在计时区间内）。
    ///
    /// 返回 `false` = 时钟倒退：本轮样本作废（调用方转 `clock_unusable`）。
    fn collect_at<R>(&mut self, ops: u64, warmup: u64, body: &mut impl FnMut() -> R) -> bool {
        self.samples.clear();
        self.baseline_samples.clear();
        self.round_medians = [0; ROUNDS];

        let mut warm_batches = warmup / ops;
        if warmup > 0 && warm_batches == 0 {
            warm_batches = 1;
        }
        warm_batches = warm_batches.min(MAX_WARMUP_BATCHES);
        for _ in 0..warm_batches {
            let _ = measure_batch(ops, body);
        }

        let mut baseline = || core::hint::black_box(0u64);
        for round in 0..ROUNDS {
            let mut round_work = [0u64; BATCHES_PER_ROUND];
            for (index, round_slot) in round_work.iter_mut().enumerate() {
                let work_first = (round + index).is_multiple_of(2);
                let pair = if work_first {
                    let work = match measure_batch(ops, body) {
                        Some(value) => value,
                        None => return false,
                    };
                    let base = match measure_batch(ops, &mut baseline) {
                        Some(value) => value,
                        None => return false,
                    };
                    (work, base)
                } else {
                    let base = match measure_batch(ops, &mut baseline) {
                        Some(value) => value,
                        None => return false,
                    };
                    let work = match measure_batch(ops, body) {
                        Some(value) => value,
                        None => return false,
                    };
                    (work, base)
                };
                self.samples.push(pair.0);
                self.baseline_samples.push(pair.1);
                *round_slot = pair.0;
            }
            round_work.sort_unstable();
            self.round_medians[round] = round_work[BATCHES_PER_ROUND / 2];
        }
        true
    }

    fn fail_unusable(&mut self) {
        self.status = BenchStatus::ClockUnusable;
        self.samples.clear();
        self.baseline_samples.clear();
        self.round_medians = [0; ROUNDS];
        self.calibration_retries = 0;
    }

    pub fn finish(mut self) -> BenchResult {
        let paired = paired_diff_median(&self.samples, &self.baseline_samples);
        self.samples.sort_unstable();
        self.baseline_samples.sort_unstable();
        let stats = summarize(&self.samples, self.plan.target);
        let baseline = summarize(&self.baseline_samples, self.plan.target);
        let mean = stats.total.checked_div(stats.samples).unwrap_or(0);
        BenchResult {
            name: self.name,
            status: self.status,
            plan: self.plan,
            operations_per_batch: self.plan.calibration.ops_per_batch,
            calibration_retries: self.calibration_retries,
            rounds: ROUNDS,
            batches_per_round: BATCHES_PER_ROUND,
            samples: stats.samples,
            iterations: self
                .plan
                .calibration
                .ops_per_batch
                .saturating_mul(stats.samples),
            mean,
            stats,
            round_medians: self.round_medians,
            baseline,
            paired_diff_median: paired,
            resolution_limited: resolution_limited(
                self.plan.target,
                self.plan.cap,
                stats.below_floor,
            ),
        }
    }
}

/// 一步到位：校准 + warmup + 采集 + 聚合。
pub fn run<R>(name: &'static str, warmup: u64, body: impl FnMut() -> R) -> BenchResult {
    let mut bench = Bench::new(name);
    bench.run(warmup, body);
    bench.finish()
}

/// 恰好两次读钟夹住 `operations` 次 body 调用（结果消耗在计时区间内）。
///
/// 返回 `None` = 时钟倒退：不掩盖（调用方转 `clock_unusable`），
/// 也不做饱和减法把负差值伪装成 0。
fn measure_batch<R>(operations: u64, body: &mut impl FnMut() -> R) -> Option<u64> {
    let start = now();
    let mut index = 0;
    while index < operations {
        core::hint::black_box(body());
        index += 1;
    }
    now().checked_sub(start)
}

/// 配对差值（work - baseline，batch 总时长）的中位数（顺序必须一一对应）。
///
/// 小 / 负的差值表示增量成本**无法分辨**：不做"独立最小值相减"，
/// 也不 clamp 到 0。
fn paired_diff_median(work: &[u64], baseline: &[u64]) -> Option<i64> {
    if work.is_empty() || work.len() != baseline.len() {
        return None;
    }
    let mut differences: Vec<i64> = work
        .iter()
        .zip(baseline)
        .map(|(&w, &b)| w as i64 - b as i64)
        .collect();
    differences.sort_unstable();
    Some(differences[differences.len() / 2])
}

#[cfg(test)]
mod tests;
