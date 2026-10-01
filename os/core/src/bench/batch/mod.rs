//! 批量计时的公共 policy：时钟刻画、K 校准、batch 统计。
//!
//! 这里的函数只依赖**注入的时钟读数 / pilot 闭包**，不碰 Console、trace 或
//! 具体被测体，所以可以在 host test 里用合成数据直接验证（docs/development/testing.md §1）。

use super::{ClockUnit, clock_unit, now};
use alloc::vec::Vec;

/// 时钟探测的最大读次数（有界：时钟不前进也一定结束）。
pub const CLOCK_PROBE_READS: u64 = 1024;

/// bracket 开销估计用的空 batch 次数。
pub const BRACKET_SAMPLES: u64 = 64;

/// 采集低于校准 floor 时允许重采的次数上限（每次翻倍 K）。
///
/// 重试是为了兑现 spec：低于 floor 的采集要如实保留、计数，并在 bounds
/// 允许时用更大的 K 重采；但重试本身必须有界（最多翻 3 次 = 最多到 8K）。
pub const MAX_RETRIES: u64 = 3;

/// 单批时长上限：1 ms（按当前时钟单位换算的 policy 参数，不是普适常数）。
pub fn batch_cap() -> u64 {
    match clock_unit() {
        ClockUnit::Nanoseconds => 1_000_000,
        ClockUnit::TimebaseTicks => {
            // 10 MHz timebase → 1 ms = 10_000 tick；速率未知时用同一缺省
            // （bench-only 采样参数，不是对机器真相的替代）。
            let hz = crate::machine::committed()
                .and_then(|info| info.timebase_frequency)
                .map_or(10_000_000, |hz| hz.get());
            (hz / 1_000).max(1)
        }
    }
}

/// 有界时钟探测的结果（单位 = 当前时钟单位）。
///
/// 三个量刻意分开，**不可互相冒充**：
/// - `min_positive`：观测到的最小正 delta —— 包含读钟调用开销，不是分辨率证明；
/// - `median_positive`：正样本中位数 —— back-to-back 读钟成本的典型量级；
/// - `zero_deltas` / `backwards`：时钟不前进或倒退的观测数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockProbe {
    pub reads: u64,
    pub zero_deltas: u64,
    pub backwards: u64,
    pub min_positive: u64,
    pub median_positive: u64,
}

impl ClockProbe {
    /// 时钟是否真的前进过（并且没有倒退）。
    pub const fn progressed(self) -> bool {
        self.min_positive != 0 && self.backwards == 0
    }
}

/// 有界时钟探测：连续读钟 `max_reads` 次，记录零 delta / 倒退 / 最小与典型正 delta。
pub fn probe_clock(max_reads: u64) -> ClockProbe {
    let mut positives = Vec::new();
    let mut prev = now();
    let mut probe = ClockProbe::default();
    while probe.reads < max_reads {
        let next = now();
        probe.reads += 1;
        match next.checked_sub(prev) {
            None => probe.backwards += 1,
            Some(0) => probe.zero_deltas += 1,
            Some(delta) => positives.push(delta),
        }
        prev = next;
    }
    positives.sort_unstable();
    if let Some(&min) = positives.first() {
        probe.min_positive = min;
    }
    if !positives.is_empty() {
        probe.median_positive = positives[positives.len() / 2];
    }
    probe
}

/// bracket 开销下界：空 batch（只有 black_box，没有实际工作）的最小时长。
///
/// 取最小值是一致的选择：干扰只会把时间加上去。它回答的是「一次 bracket
/// 至少多大」，不是时钟分辨率。
pub fn bracket_cost_min(samples: u64) -> u64 {
    let mut best = u64::MAX;
    let mut index = 0;
    while index < samples {
        let start = now();
        core::hint::black_box(0u64);
        if let Some(delta) = now().checked_sub(start) {
            best = best.min(delta);
        }
        index += 1;
    }
    if best == u64::MAX { 0 } else { best }
}

/// 校准目标批时长：`max(200 * q, 100 * bracket)`（policy 参数）。
///
/// `q` 是标称量子（10 MHz timebase 上 1 tick = 100 ns），`bracket` 是测到的
/// 括号开销。两个系数是**实验起点**，写进报告供读者质疑。
pub const fn calibration_target(quantum: u64, bracket: u64) -> u64 {
    let by_quantum = 200 * quantum;
    let by_bracket = 100 * bracket;
    if by_quantum > by_bracket {
        by_quantum
    } else {
        by_bracket
    }
}

/// K 选择结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Calibration {
    pub ops_per_batch: u64,
    pub target: u64,
    pub cap: u64,
    /// target 本身超过 cap：任何合法 K 都到不了"够用"的批时长（如实上报）。
    pub resolution_limited: bool,
}

/// 纯函数：从 K=1 开始翻倍选每批操作数。
///
/// `pilot(k)` 必须返回该 K 下 **3 个 pilot batch 的中位数**（避免用一次宿主
/// 停顿决定 K）。取满足 cap 的最大 K；达到 target 或 pilot 超过 cap 即停。
/// 每次翻倍都是有界探测，不会无限自旋。
///
/// **K=1 的 pilot 不作为停止依据**：冷启动 / 一次宿主停顿足以让 K=1 的中位数
/// 虚高，制造假 overshoot 把 K 钉死在 1（重采时 153/155 个 batch 都低于
/// floor，测量报废）。因此先向上探测 ≥ 一个候选 K，再由采集期的 below-floor
/// 重试（[`retry_ops_per_batch`]）验证 / 修正最终 K。
pub fn choose_ops_per_batch(
    target: u64,
    cap: u64,
    max_ops: u64,
    mut pilot: impl FnMut(u64) -> u64,
) -> Calibration {
    let max_ops = max_ops.max(1);
    let mut best = 1u64;
    let mut ops = 1u64;
    while ops <= max_ops {
        let ticks = pilot(ops);
        if ticks <= cap {
            best = ops;
        }
        // ops == 1 时两个停止条件都不采信（只有它的中位数单独说过话）；
        // 从 K=2 起恢复正常停止规则。
        if ops > 1 && (ticks >= target || ticks > cap) {
            break;
        }
        match ops.checked_mul(2) {
            Some(next) => ops = next,
            None => break,
        }
    }
    Calibration {
        ops_per_batch: best,
        target,
        cap,
        resolution_limited: target > cap,
    }
}

/// 低于 floor 的重试决策（纯函数：pilot 注入，host test 可确定复现）。
///
/// 返回 `Some(next_ops)` = 应在 `2K` 重采；`None` = 停止重试，最终结果按
/// 最后一轮如实上报。停止条件：
/// - 本轮没有 below-floor 批次（不需要重试）；
/// - 已重试 [`MAX_RETRIES`] 次（有界终止）；
/// - `2K` 超过 `max_ops`（单批操作数硬上限）；
/// - `2K` 的 pilot 超过 `cap`（重采会越过 1 ms 批时长约束）。
pub fn retry_ops_per_batch(
    ops: u64,
    retries: u64,
    below_floor: u64,
    cap: u64,
    max_ops: u64,
    mut pilot: impl FnMut(u64) -> u64,
) -> Option<u64> {
    if below_floor == 0 || retries >= MAX_RETRIES {
        return None;
    }
    let next = ops.checked_mul(2)?;
    if next > max_ops.max(1) {
        return None;
    }
    if pilot(next) > cap {
        return None;
    }
    Some(next)
}

/// 最终 resolution 判定（纯函数）：`target > cap`（校准本身无法达到目标），
/// 或**重试已用尽**而最后一轮仍有低于 floor 的批次（`below_floor > 0`）。
///
/// 干净收敛的采集（`below_floor == 0`）即使经历过重试也不 restricted。
pub const fn resolution_limited(target: u64, cap: u64, below_floor: u64) -> bool {
    target > cap || below_floor > 0
}

/// 三个值的中位数（pilot 用；不排序、不分配）。
pub const fn median3(a: u64, b: u64, c: u64) -> u64 {
    if a > b {
        if b > c {
            b
        } else if a > c {
            c
        } else {
            a
        }
    } else if a > c {
        a
    } else if b > c {
        c
    } else {
        b
    }
}

/// 一次 primitive 的测量计划（完整 policy 快照，报告里全部写出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeasurementPlan {
    pub probe: ClockProbe,
    pub bracket_cost: u64,
    pub quantum: u64,
    pub target: u64,
    pub cap: u64,
    pub calibration: Calibration,
    /// 时钟可用且 bracket 可测 —— 否则不测，直接报 `clock_unusable`。
    pub usable: bool,
}

impl MeasurementPlan {
    /// `Bench::new` 到 `Bench::run` 之间的占位计划。
    pub const fn unmeasured() -> Self {
        Self {
            probe: ClockProbe {
                reads: 0,
                zero_deltas: 0,
                backwards: 0,
                min_positive: 0,
                median_positive: 0,
            },
            bracket_cost: 0,
            quantum: 1,
            target: 0,
            cap: 0,
            calibration: Calibration {
                ops_per_batch: 1,
                target: 0,
                cap: 0,
                resolution_limited: false,
            },
            usable: false,
        }
    }
}

/// 由探测结果得到测量计划（纯函数：pilot 注入，便于 host test）。
///
/// `bracket_cost == 0`（空 batch 没能前进一个 tick）时退回 `probe.min_positive`；
/// 两者都不可用 → `usable = false`。
pub fn measurement_plan(
    probe: ClockProbe,
    bracket_cost: u64,
    cap: u64,
    max_ops: u64,
    pilot: impl FnMut(u64) -> u64,
) -> MeasurementPlan {
    let bracket = if bracket_cost == 0 {
        probe.min_positive
    } else {
        bracket_cost
    };
    let quantum = 1;
    let target = calibration_target(quantum, bracket);
    let usable = probe.progressed() && bracket > 0;
    let calibration = if usable {
        choose_ops_per_batch(target, cap, max_ops, pilot)
    } else {
        Calibration {
            ops_per_batch: 1,
            target,
            cap,
            resolution_limited: target > cap,
        }
    };
    MeasurementPlan {
        probe,
        bracket_cost: bracket,
        quantum,
        target,
        cap,
        calibration,
        usable,
    }
}

/// batch 总时长的聚合统计（分母是 `operations_per_batch`，换算由报告工具做）。
///
/// `below_floor` 保留低于校准目标的观测原始值并计数 —— **不静默丢弃**。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatchStats {
    pub samples: u64,
    pub total: u64,
    pub min: u64,
    pub median: u64,
    pub p95: u64,
    pub max: u64,
    pub below_floor: u64,
}

/// 聚合已升序排序的 batch 样本。`floor` 是校准目标（低于它记入 below_floor）。
pub fn summarize(sorted: &[u64], floor: u64) -> BatchStats {
    if sorted.is_empty() {
        return BatchStats::default();
    }
    let mut total = 0u64;
    let mut below = 0u64;
    for &value in sorted {
        total = total.saturating_add(value);
        if value < floor {
            below += 1;
        }
    }
    BatchStats {
        samples: sorted.len() as u64,
        total,
        min: sorted[0],
        median: percentile(sorted, 50),
        p95: percentile(sorted, 95),
        max: sorted[sorted.len() - 1],
        below_floor: below,
    }
}

/// 最近秩百分位：`rank = ceil(n * p / 100)`（1-based），空样本返回 0。
fn percentile(sorted: &[u64], p: u64) -> u64 {
    let n = sorted.len() as u64;
    let rank = (n * p).div_ceil(100);
    let index = rank.saturating_sub(1).min(n - 1) as usize;
    sorted[index]
}

#[cfg(test)]
mod tests;
