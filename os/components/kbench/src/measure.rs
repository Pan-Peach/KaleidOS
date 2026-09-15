//! 批量计时核心（no_std、无分配）：时钟探测、K 校准、batch 采集。
//!
//! 与 host 的 `os/core/src/bench/batch.rs` 是**同一套 policy 的镜像**：组件不能
//! 依赖 kernel crate，所以这里用固定长度的数组（`SAMPLES = 5 × 31 = 155`）替代
//! `Vec`，样本量有界且全部保留。样本**聚合**（`summarize` / `sort` /
//! `paired_diff_median`）在 `crate::stats`。
//!
//! 组件内不做除法：p95 下标在样本数固定时是常量；`total`/`samples` 作为原始
//! 分子分母报出，`ns/op = d * 10^9 / (K * f)` 由 host 先乘后除。
//!
//! 一次 primitive 的顺序固定为 **warmup（K=1）→ 校准 K → 采集 →（低于 floor
//! 时）有界重采**：冷启动样本不能决定 K；低于 floor 的采集在有界范围内用更大的
//! K 重采，只有重试用尽（或 `target > cap`）才如实标 resolution_limited。

use crate::{BATCHES_PER_ROUND, ROUNDS, SAMPLES};
use kcomp_sdk::abi;

/// 时钟探测中最多保留多少个正 delta 用来算"典型读钟成本"。
const DELTA_SAMPLES: usize = 256;

/// 采集低于校准 floor 时允许重采的次数上限（每次翻倍 K，与 host 同值）。
const MAX_RETRIES: u64 = 3;

/// 有界时钟探测结果（与 host 的同名类型同语义）。
#[derive(Clone, Copy)]
pub(crate) struct ClockProbe {
    pub reads: u64,
    pub zero_deltas: u64,
    pub backwards: u64,
    pub min_positive: u64,
    pub median_positive: u64,
}

impl ClockProbe {
    pub fn progressed(&self) -> bool {
        self.min_positive != 0 && self.backwards == 0
    }
}

/// 有界探测：连续读钟 `max_reads` 次，记录零 delta / 倒退 / 最小与典型正 delta。
pub(crate) fn probe_clock(max_reads: u64) -> ClockProbe {
    let mut deltas = [0u64; DELTA_SAMPLES];
    let mut count = 0usize;
    let mut probe = ClockProbe {
        reads: 0,
        zero_deltas: 0,
        backwards: 0,
        min_positive: 0,
        median_positive: 0,
    };
    let mut prev = unsafe { abi::kcore_now() };
    while probe.reads < max_reads {
        let next = unsafe { abi::kcore_now() };
        probe.reads += 1;
        match next.checked_sub(prev) {
            None => probe.backwards += 1,
            Some(0) => probe.zero_deltas += 1,
            Some(delta) => {
                if probe.min_positive == 0 || delta < probe.min_positive {
                    probe.min_positive = delta;
                }
                if count < DELTA_SAMPLES {
                    deltas[count] = delta;
                    count += 1;
                }
            }
        }
        prev = next;
    }
    if count > 0 {
        deltas[..count].sort_unstable();
        probe.median_positive = deltas[count / 2];
    }
    probe
}

/// bracket 开销下界：空 batch 的最小时长（干扰只会加时间）。
pub(crate) fn bracket_cost_min(samples: u64) -> u64 {
    let mut best = u64::MAX;
    let mut index = 0;
    while index < samples {
        let start = unsafe { abi::kcore_now() };
        core::hint::black_box(0u64);
        if let Some(delta) = unsafe { abi::kcore_now() }.checked_sub(start)
            && delta < best
        {
            best = delta;
        }
        index += 1;
    }
    if best == u64::MAX { 0 } else { best }
}

/// 1 ms 对应的 timebase tick 数（`hz / 1000`）—— **不用除法**。
///
/// RV32 上 `u64 / u64` 会落到 `__udivdi3` libcall（组件明确禁止，见模块头）；
/// 这里用二分答案 + `checked_mul` 溢出保护，只在 init 期调用一次。
pub(crate) fn ticks_per_millisecond(timebase_hz: u64) -> u64 {
    const DIVISOR: u64 = 1_000;
    let mut low = 0u64;
    let mut high = timebase_hz;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        match middle.checked_mul(DIVISOR) {
            Some(product) if product <= timebase_hz => low = middle,
            _ => high = middle - 1,
        }
    }
    low
}

/// 校准目标批时长：`max(200 * q, 100 * bracket)`。
pub(crate) const fn calibration_target(quantum: u64, bracket: u64) -> u64 {
    let by_quantum = 200 * quantum;
    let by_bracket = 100 * bracket;
    if by_quantum > by_bracket {
        by_quantum
    } else {
        by_bracket
    }
}

/// 三个值的中位数（pilot 用；避免用一次宿主停顿决定 K）。
pub(crate) const fn median3(a: u64, b: u64, c: u64) -> u64 {
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

/// 从 K=1 翻倍选每批操作数；`pilot(k)` 返回该 K 下 3 个 pilot batch 的中位数。
///
/// **K=1 的 pilot 不作为停止依据**（与 host 同语义）：冷启动 / 一次宿主停顿
/// 足以让 K=1 的中位数虚高，制造假 overshoot 把 K 钉死在 1；先向上探测 ≥ 一个
/// 候选 K，再由采集期的 below-floor 重试验证 / 修正最终 K。
pub(crate) fn choose_ops_per_batch(
    target: u64,
    cap: u64,
    max_ops: u64,
    pilot: &mut impl FnMut(u64) -> u64,
) -> u64 {
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
    best
}

/// 低于 floor 的重试决策（与 host 的 `retry_ops_per_batch` 同语义）。
///
/// 返回 `Some(next)` = 应在 `2K` 重采；`None` = 停止重试，最终结果按最后一轮
/// 如实上报：没有 below-floor 批次 / 重试已到 [`MAX_RETRIES`] / `2K` 超过
/// `max_ops` / `2K` 的 pilot 超过 cap。
pub(crate) fn retry_ops_per_batch(
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

/// 固定 batch 数的 warmup（不计入结果）。参数是 batch 数而不是操作数：
/// 组件内不做除法（warmup 换算也不下沉到被测端）。
pub(crate) fn warmup<F: FnMut() -> u64>(ops: u64, batches: u64, body: &mut F) {
    let mut index = 0;
    while index < batches {
        let _ = batch(ops, body);
        index += 1;
    }
}

/// 该 K 下 3 个 pilot batch 的中位数（独立函数：采集 / 重试之间复用，
/// 不把 `body` 的可变借用困在闭包里）。
pub(crate) fn pilot_at<F: FnMut() -> u64>(ops: u64, body: &mut F) -> u64 {
    let a = batch(ops, body).unwrap_or(u64::MAX);
    let b = batch(ops, body).unwrap_or(u64::MAX);
    let c = batch(ops, body).unwrap_or(u64::MAX);
    median3(a, b, c)
}

/// 恰好两次读钟夹住 `ops` 次 body 调用（结果消耗在计时区间内）。
/// `None` = 时钟倒退：不掩盖，调用方转 `clock_unusable`。
pub(crate) fn batch<F: FnMut() -> u64>(ops: u64, body: &mut F) -> Option<u64> {
    let start = unsafe { abi::kcore_now() };
    let mut index = 0;
    while index < ops {
        core::hint::black_box(body());
        index += 1;
    }
    unsafe { abi::kcore_now() }.checked_sub(start)
}

/// 一次 primitive 的全部采集结果（有界数组，约 2.5 KB）。
///
/// **不放在任务栈上**：组件任务内核栈只有 4 KiB（`task::TaskTable::create`），
/// 2.5 KB 的样本缓冲会挤掉调用链余量。由调用方提供存放位置（组件用静态
/// buffer），[`collect_into`] 写入其中。
pub(crate) struct Collected {
    pub work: [u64; SAMPLES],
    pub baseline: [u64; SAMPLES],
    pub round_medians: [u64; ROUNDS],
}

impl Collected {
    /// 全零缓冲（静态初始化用；`collect_into` 每次覆写全部字段）。
    pub const fn new() -> Self {
        Self {
            work: [0; SAMPLES],
            baseline: [0; SAMPLES],
            round_medians: [0; ROUNDS],
        }
    }
}

/// 冻结 K 采集：warmup（固定 batch 数，不计入）→ `phase()` → 每轮交替
/// work / null baseline，结果写入调用方的 `target`。
///
/// `warmup_batches` 用 batch 数而不是操作数：组件内不做除法（连 warmup 换算也
/// 不下沉到被测端）。
///
/// `phase()` 在内部 warmup 之后、正式采样之前调用一次：sched primitive 用它把
/// "按样本计的 handoff 计数"清零，使报告口径 = 正式样本（而不是把 warmup 也算进
/// `handoff_count`）。其它 primitive 传空实现。
///
/// 返回 `false` = 时钟倒退（不掩盖；调用方转 `clock_unusable`）。
pub(crate) fn collect_into<W: FnMut() -> u64, B: FnMut() -> u64, P: FnMut()>(
    ops: u64,
    warmup_batches: u64,
    work: &mut W,
    baseline: &mut B,
    target: &mut Collected,
    phase: &mut P,
) -> bool {
    target.work = [0; SAMPLES];
    target.baseline = [0; SAMPLES];
    target.round_medians = [0; ROUNDS];

    warmup(ops, warmup_batches, work);
    phase();

    let mut sample = 0usize;
    let mut round = 0usize;
    while round < ROUNDS {
        let mut round_work = [0u64; BATCHES_PER_ROUND];
        let mut batch_index = 0usize;
        while batch_index < BATCHES_PER_ROUND {
            // 交替顺序，配对差值不受单调漂移的系统性影响。
            let work_first = (round + batch_index).is_multiple_of(2);
            let pair = if work_first {
                (batch(ops, work), batch(ops, baseline))
            } else {
                let base = batch(ops, baseline);
                let work_value = batch(ops, work);
                (work_value, base)
            };
            let (Some(work_value), Some(base_value)) = pair else {
                return false;
            };
            target.work[sample] = work_value;
            target.baseline[sample] = base_value;
            round_work[batch_index] = work_value;
            sample += 1;
            batch_index += 1;
        }
        round_work.sort_unstable();
        target.round_medians[round] = round_work[BATCHES_PER_ROUND / 2];
        round += 1;
    }
    true
}
