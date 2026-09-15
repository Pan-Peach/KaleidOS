use super::*;

fn probe_with(min_positive: u64) -> ClockProbe {
    ClockProbe {
        reads: 8,
        zero_deltas: 1,
        backwards: 0,
        min_positive,
        median_positive: min_positive,
    }
}

#[test]
fn probe_clock_is_bounded_and_separates_delta_kinds() {
    let probe = probe_clock(CLOCK_PROBE_READS);
    assert_eq!(probe.reads, CLOCK_PROBE_READS, "探测必须有界");
    if probe.min_positive == 0 {
        // 时钟整段没动：如实归类为不可用，不伪装成 0 成本。
        assert_eq!(probe.zero_deltas, CLOCK_PROBE_READS);
        assert!(!probe.progressed());
    } else {
        assert!(probe.progressed());
        assert!(probe.median_positive >= probe.min_positive);
    }
}

#[test]
fn median3_returns_the_middle_value() {
    assert_eq!(median3(9, 1, 5), 5);
    assert_eq!(median3(1, 9, 5), 5);
    assert_eq!(median3(5, 1, 9), 5);
    assert_eq!(median3(1, 1, 9), 1);
    assert_eq!(median3(9, 9, 1), 9);
}

#[test]
fn calibration_doubles_until_target_then_stops() {
    // 每 op 10 tick：K=128 时 batch = 1280 tick，第一次 >= target=1000。
    let calibration = choose_ops_per_batch(1000, 1_000_000, 1 << 20, |ops| ops * 10);
    assert_eq!(calibration.ops_per_batch, 128);
    assert!(!calibration.resolution_limited);
}

#[test]
fn calibration_never_picks_a_batch_over_the_cap() {
    // 每 op 1000 tick：K=8 时 8000 <= cap=10000，K=16 时 16000 > cap。
    let calibration = choose_ops_per_batch(1_000_000, 10_000, 1 << 20, |ops| ops * 1_000);
    assert_eq!(calibration.ops_per_batch, 8, "超过 cap 的 K 必须回退");
}

#[test]
fn calibration_reports_when_target_exceeds_cap() {
    let calibration = choose_ops_per_batch(2_000_000, 1_000_000, 1 << 20, |ops| ops);
    assert!(calibration.resolution_limited, "target > cap 必须如实上报");
    assert!(
        calibration.ops_per_batch <= calibration.cap,
        "仍然只取不超 cap 的 K"
    );
}

#[test]
fn inflated_first_pilot_never_pins_calibration_to_one() {
    // 冷启动 / 宿主停顿能把 K=1 的 pilot 抬到 target 之上，甚至抬过 cap；
    // 两种假 overshoot 都不能把 K 钉死在 1（真正的验证留给采集 + 重试）。
    for inflated in [5_000u64, 50_000] {
        let calibration = choose_ops_per_batch(200, 10_000, 1 << 20, |ops| {
            if ops == 1 { inflated } else { ops * 10 }
        });
        assert_eq!(
            calibration.ops_per_batch, 32,
            "inflated K=1 pilot {inflated} 不能钉死 K=1"
        );
    }
}

#[test]
fn below_floor_collection_triggers_a_larger_k_retry() {
    // 低于 floor 的采集必须重采更大的 K（翻倍）；干净的采集不重试。
    assert_eq!(
        retry_ops_per_batch(64, 0, 1, 10_000, 1 << 20, |ops| ops * 10),
        Some(128)
    );
    assert_eq!(
        retry_ops_per_batch(64, 0, 0, 10_000, 1 << 20, |ops| ops * 10),
        None,
        "没有 below-floor 批次就不重试"
    );
}

#[test]
fn retries_are_bounded_by_count_and_ops_ceiling() {
    // 次数用尽 → 停（最终结果按最后一轮如实报）。
    assert_eq!(
        retry_ops_per_batch(8, MAX_RETRIES, 1, 10_000, 1 << 20, |ops| ops * 10),
        None
    );
    // 翻倍会越过 MAX_OPS_PER_BATCH → 停。
    assert_eq!(
        retry_ops_per_batch(
            crate::bench::MAX_OPS_PER_BATCH,
            0,
            1,
            u64::MAX,
            crate::bench::MAX_OPS_PER_BATCH,
            |ops| ops * 10,
        ),
        None
    );
    // 翻倍后的 pilot 超过 cap → 停（不越过 1 ms 批时长约束）。
    assert_eq!(
        retry_ops_per_batch(64, 0, 1, 10_000, 1 << 20, |ops| ops * 1_000),
        None
    );
}

#[test]
fn retry_sequence_terminates_and_stays_honest() {
    // 每轮都低于 floor、pilot 永远合法：重试严格有界（MAX_RETRIES 次），K 逐次翻倍。
    let mut ops = 1u64;
    let mut retries = 0u64;
    while let Some(next) =
        retry_ops_per_batch(ops, retries, 7, 10_000, 1 << 20, |candidate| candidate)
    {
        ops = next;
        retries += 1;
    }
    assert_eq!(retries, MAX_RETRIES, "重试次数必须有界");
    assert_eq!(ops, 1 << MAX_RETRIES, "每次重试把 K 翻倍");
    // 重试用尽仍有 below-floor 批次 → 如实标 resolution_limited；
    // 干净收敛 / target > cap 的语义保持。
    assert!(resolution_limited(200, 10_000, 7));
    assert!(!resolution_limited(200, 10_000, 0));
    assert!(resolution_limited(20_000, 10_000, 0));
}

#[test]
fn plan_is_unusable_when_the_clock_never_advances() {
    let probe = ClockProbe {
        reads: 64,
        zero_deltas: 64,
        ..ClockProbe::default()
    };
    let plan = measurement_plan(probe, 0, 10_000, 1 << 20, |ops| ops);
    assert!(!plan.usable);
    assert_eq!(plan.calibration.ops_per_batch, 1);
}

#[test]
fn plan_falls_back_to_probe_when_bracket_is_immeasurable() {
    let probe = probe_with(7);
    let plan = measurement_plan(probe, 0, 10_000, 1 << 20, |ops| ops * 100);
    assert!(plan.usable);
    assert_eq!(plan.bracket_cost, 7, "bracket 测不到时退回最小正 delta");
    assert_eq!(plan.target, calibration_target(1, 7));
}

#[test]
fn stats_use_nearest_rank_and_keep_below_floor_observations() {
    let sorted = [10, 20, 30, 40, 100];
    let stats = summarize(&sorted, 25);
    assert_eq!(stats.samples, 5);
    assert_eq!(stats.total, 200);
    assert_eq!(stats.min, 10);
    assert_eq!(stats.median, 30);
    assert_eq!(stats.p95, 100, "p95 = rank ceil(5*0.95) = 5");
    assert_eq!(stats.max, 100);
    assert_eq!(stats.below_floor, 2, "低于 floor 的值保留并计数");
}

#[test]
fn stats_of_no_samples_are_zero_not_sentinel() {
    let stats = summarize(&[], 100);
    assert_eq!(stats, BatchStats::default());
}
