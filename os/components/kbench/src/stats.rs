//! batch 样本的聚合统计（纯逻辑，无分配；从 `measure.rs` 拆出）。
//!
//! 统计对象固定为 `SAMPLES = 5 × 31 = 155` 个 batch 总时长（原始 tick）。
//! 组件内不做除法：`p95` 下标在样本数固定时是常量；`total`/`samples` 作为原始
//! 分母报出，换算由 host 先乘后除。

use crate::SAMPLES;

/// p95 采用最近秩：rank = ceil(155 * 95 / 100) = 148（1-based）→ 下标 147。
/// 样本数固定，所以不需要除法。
const P95_INDEX: usize = (SAMPLES * 95).div_ceil(100) - 1;

/// batch 总时长的聚合统计（单位 = timebase tick，分母 = `operations_per_batch`）。
/// 样本数固定为 [`SAMPLES`]，所以不需要在 struct 里重复携带。
#[derive(Clone, Copy, Default)]
pub(crate) struct Stats {
    pub total: u64,
    pub min: u64,
    pub median: u64,
    pub p95: u64,
    pub max: u64,
    pub below_floor: u64,
}

/// 聚合已升序的 batch 样本（155 个全部保留；低于 `floor` 的原始值照计）。
pub(crate) fn summarize(sorted: &[u64; SAMPLES], floor: u64) -> Stats {
    let mut total = 0u64;
    let mut below = 0u64;
    let mut index = 0;
    while index < SAMPLES {
        let value = sorted[index];
        total = total.saturating_add(value);
        if value < floor {
            below += 1;
        }
        index += 1;
    }
    Stats {
        total,
        min: sorted[0],
        median: sorted[SAMPLES / 2],
        p95: sorted[P95_INDEX],
        max: sorted[SAMPLES - 1],
        below_floor: below,
    }
}

/// 就地排序一个 batch 样本数组（`core::slice::sort_unstable`，无分配）。
pub(crate) fn sort(samples: &mut [u64; SAMPLES]) {
    samples.sort_unstable();
}

/// 配对差值（work - baseline）的中位数；小 / 负值 = 增量成本无法分辨。
pub(crate) fn paired_diff_median(work: &[u64; SAMPLES], baseline: &[u64; SAMPLES]) -> i64 {
    let mut differences = [0i64; SAMPLES];
    let mut index = 0;
    while index < SAMPLES {
        differences[index] = work[index] as i64 - baseline[index] as i64;
        index += 1;
    }
    differences.sort_unstable();
    differences[SAMPLES / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1..=155（已升序），便于对固定下标中位数 / p95 做断言。
    fn ascending() -> [u64; SAMPLES] {
        let mut samples = [0u64; SAMPLES];
        let mut index = 0usize;
        while index < SAMPLES {
            samples[index] = index as u64 + 1;
            index += 1;
        }
        samples
    }

    #[test]
    fn summarize_brackets_the_observations() {
        let stats = summarize(&ascending(), 100);
        assert_eq!(stats.min, 1);
        assert_eq!(stats.max, 155);
        assert_eq!(stats.median, 78);
        assert_eq!(stats.p95, 148);
        assert_eq!(stats.below_floor, 99, "低于 floor 的原始值照计");
        assert_eq!(stats.total, 12_090);
    }

    #[test]
    fn sort_orders_ascending() {
        let mut samples = [0u64; SAMPLES];
        samples[0] = 9;
        samples[1] = 1;
        samples[SAMPLES - 1] = 5;
        sort(&mut samples);
        assert_eq!(samples[0], 0);
        assert_eq!(samples[SAMPLES - 3], 1);
        assert_eq!(samples[SAMPLES - 2], 5);
        assert_eq!(samples[SAMPLES - 1], 9);
    }
}
