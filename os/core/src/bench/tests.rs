use super::*;

#[test]
fn batch_samples_are_bounded_and_fully_retained() {
    let mut bench = Bench::new("bounded");
    bench.run(0, || core::hint::black_box(0u8));
    assert_eq!(bench.samples.len(), SAMPLE_CAP, "样本必须有界");
    assert_eq!(bench.baseline_samples.len(), SAMPLE_CAP);
    let result = bench.finish();
    assert_eq!(result.samples, SAMPLE_CAP as u64, "百分位覆盖全部样本");
    assert_eq!(result.rounds, ROUNDS);
    assert_eq!(result.batches_per_round, BATCHES_PER_ROUND);
}

#[test]
fn warmup_is_not_counted() {
    let result = run("warmup", 500, || core::hint::black_box(0u8));
    assert_eq!(
        result.iterations,
        result.operations_per_batch * result.samples,
        "只统计测量到的 batch，warmup 不计入"
    );
}

#[test]
fn no_iterations_yields_zeroes_not_garbage() {
    let result = Bench::new("empty").finish();
    assert_eq!(result.iterations, 0);
    assert_eq!(result.stats.total, 0);
    assert_eq!(result.stats.min, 0, "未采样时 min 不能是它自己的哨兵值");
    assert_eq!(result.mean, 0);
    assert_eq!(result.stats.median, 0);
    assert_eq!(result.paired_diff_median, None);
}

#[test]
fn host_clock_is_nanoseconds_and_monotonic() {
    assert_eq!(clock_unit(), ClockUnit::Nanoseconds);
    let a = now();
    let b = now();
    assert!(b >= a, "时钟必须单调不降");
}

#[test]
fn paired_difference_median_keeps_its_sign() {
    assert_eq!(paired_diff_median(&[10, 20], &[10, 20]), Some(0));
    assert_eq!(paired_diff_median(&[15, 25], &[10, 20]), Some(5));
    assert_eq!(paired_diff_median(&[5, 15], &[10, 20]), Some(-5));
    assert_eq!(paired_diff_median(&[], &[]), None);
    assert_eq!(paired_diff_median(&[1], &[]), None, "长度不一致不猜");
}

#[test]
fn aggregation_brackets_the_observations() {
    let result = run("agg", 0, || core::hint::black_box(0u32));
    assert!(result.stats.min <= result.mean, "min 必须 <= mean");
    assert!(result.mean <= result.stats.max, "mean 必须 <= max");
    assert!(
        result.stats.total >= result.stats.max,
        "batch 总和至少等于单个 batch 最大值"
    );
    assert!(result.stats.median >= result.stats.min && result.stats.median <= result.stats.max);
    assert!(result.stats.p95 >= result.stats.min && result.stats.p95 <= result.stats.max);
}
