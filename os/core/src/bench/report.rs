//! 报告层：`BENCH-ENV` 环境行 + 每个 primitive 的 `BENCH <name>` / `key=value` 块。
//!
//! 纯文本、机器可解析（第一阶段不做可视化、不做总分）。所有时间量都是
//! **原始整数**（batch 总时长，单位见 `unit=`）；小数换算由 host 报告工具做：
//! `ns/op = d * 10^9 / (K * f)` —— **先乘后除**，不在测量端截断。
//!
//! 报告里的统计对象是 **batch 总时长**（分母 `operations_per_batch`）：
//! batch p95 **不是**单次操作 p95（见 docs/benchmark.md）。

use super::{BatchStats, MeasurementPlan, ROUNDS, clock_source, clock_unit};
use core::fmt;

/// 一次 benchmark 的最终状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchStatus {
    Ok,
    /// 时钟不前进 / 倒退：不做测量，如实上报（绝不硬报数字）。
    ClockUnusable,
}

impl BenchStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            BenchStatus::Ok => "ok",
            BenchStatus::ClockUnusable => "clock_unusable",
        }
    }
}

/// 一次 benchmark 的聚合结果。单位见 [`clock_unit`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchResult {
    pub name: &'static str,
    pub status: BenchStatus,
    pub plan: MeasurementPlan,
    /// 每批操作数 K（batch 总时长的分母）；低于 floor 的重试后会修正为最终 K。
    pub operations_per_batch: u64,
    /// 采集低于校准 floor 后执行的翻倍重采次数（有界 [`MAX_RETRIES`]）。
    pub calibration_retries: u64,
    pub rounds: usize,
    pub batches_per_round: usize,
    /// 保留的 batch 样本数（有界，且覆盖**全部**测量）。
    pub samples: u64,
    /// 测量到的总操作数 = `operations_per_batch * samples`。
    pub iterations: u64,
    /// batch 总时长的算术平均（host 侧可直接除）。
    pub mean: u64,
    pub stats: BatchStats,
    /// 每轮 batch 中位数（看轮间散布；散布大 = 有干扰）。
    pub round_medians: [u64; ROUNDS],
    /// 同一 K 交替测出的 null baseline（只有循环 / black_box，没有实际工作）。
    pub baseline: BatchStats,
    /// 配对差值（work - baseline，batch 总时长）的中位数；小 / 负值表示
    /// 增量成本**无法分辨**，不做 clamp、不做"修正"。
    pub paired_diff_median: Option<i64>,
    /// target > cap，或重试已用尽仍有低于校准 floor 的 batch（原始值仍保留在样本里）。
    pub resolution_limited: bool,
}

impl BenchResult {
    /// 机器可解析的报告（`BENCH <name>` 行 + 后续 `key=value` 行）。
    pub fn report(&self) {
        crate::printk!("{}", self);
    }
}

impl fmt::Display for BenchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "BENCH {}", self.name)?;
        writeln!(f, "method=batch")?;
        writeln!(f, "unit={}", clock_unit().as_str())?;
        writeln!(f, "sample_unit=batch_total")?;
        writeln!(f, "operations_per_batch={}", self.operations_per_batch)?;
        writeln!(f, "rounds={}", self.rounds)?;
        writeln!(f, "batches_per_round={}", self.batches_per_round)?;
        writeln!(f, "iterations={}", self.iterations)?;
        writeln!(f, "samples={}", self.samples)?;
        writeln!(f, "clock_quantum={}", self.plan.quantum)?;
        writeln!(f, "clock_probe_reads={}", self.plan.probe.reads)?;
        writeln!(f, "clock_zero_deltas={}", self.plan.probe.zero_deltas)?;
        writeln!(f, "clock_backwards={}", self.plan.probe.backwards)?;
        writeln!(
            f,
            "clock_observed_min_delta={}",
            self.plan.probe.min_positive
        )?;
        writeln!(
            f,
            "clock_median_read_delta={}",
            self.plan.probe.median_positive
        )?;
        writeln!(f, "clock_bracket_min={}", self.plan.bracket_cost)?;
        writeln!(f, "calibration_target={}", self.plan.target)?;
        writeln!(f, "batch_cap={}", self.plan.cap)?;
        writeln!(f, "calibration_retries={}", self.calibration_retries)?;
        writeln!(f, "min={}", self.stats.min)?;
        writeln!(f, "median={}", self.stats.median)?;
        writeln!(f, "mean={}", self.mean)?;
        writeln!(f, "p95={}", self.stats.p95)?;
        writeln!(f, "max={}", self.stats.max)?;
        writeln!(f, "total={}", self.stats.total)?;
        for (round, median) in self.round_medians.iter().enumerate() {
            writeln!(f, "round_{round}_median={median}")?;
        }
        writeln!(f, "below_floor_batches={}", self.stats.below_floor)?;
        writeln!(
            f,
            "resolution_limited={}",
            if self.resolution_limited { "yes" } else { "no" }
        )?;
        writeln!(f, "baseline=paired_null")?;
        writeln!(f, "baseline_min={}", self.baseline.min)?;
        writeln!(f, "baseline_median={}", self.baseline.median)?;
        writeln!(f, "baseline_p95={}", self.baseline.p95)?;
        writeln!(f, "baseline_max={}", self.baseline.max)?;
        writeln!(f, "baseline_total={}", self.baseline.total)?;
        match self.paired_diff_median {
            Some(difference) => writeln!(f, "baseline_paired_diff_median={difference}")?,
            None => writeln!(f, "baseline_paired_diff_median=n/a")?,
        }
        writeln!(f, "status={}", self.status.as_str())?;
        Ok(())
    }
}

/// 报告测量环境 —— **没有这一行，数字不可比**。
///
/// 记录：arch / XLEN / timebase / profile（privilege+vm+开关）/ 时钟来源 /
/// build mode / git commit。platform 如实写 `undetected`：Core 没有运行时
/// 板级探测，QEMU 与真机的区分由 runner 负责。加速器（TCG/KVM）同样由
/// runner 在解析时补充，Core 不猜。
pub fn report_environment() {
    const ARCH: &str = if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else if cfg!(target_arch = "riscv32") {
        "riscv32"
    } else {
        "host"
    };
    let timebase_hz = crate::machine::committed().map_or(0, |info| info.timebase_frequency);
    crate::printk!(
        "BENCH-ENV arch={} xlen={} platform=undetected timebase_hz={} privilege={} vm={} trace={} preempt={} clock={} mode={} commit={}\n",
        ARCH,
        core::mem::size_of::<usize>() * 8,
        timebase_hz,
        if cfg!(feature = "supervisor") {
            "supervisor"
        } else if cfg!(feature = "machine") {
            "machine"
        } else {
            "none"
        },
        if cfg!(feature = "vm-mmu") {
            "mmu"
        } else if cfg!(feature = "vm-nommu") {
            "nommu"
        } else {
            "none"
        },
        if cfg!(feature = "trace") { "on" } else { "off" },
        if cfg!(feature = "preempt") {
            "on"
        } else {
            "off"
        },
        clock_source(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        option_env!("KALEIDOS_GIT_COMMIT").unwrap_or("unknown"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::format;
    use alloc::string::String;

    /// 测试用最小解析器：报告必须是 `BENCH <name>` + 一组 `key=value`。
    fn parse_report(text: &str) -> BTreeMap<String, String> {
        let mut lines = text.lines();
        let header = lines.next().unwrap_or("");
        assert!(header.starts_with("BENCH "), "报告必须以 BENCH <name> 开头");
        let mut map = BTreeMap::new();
        for line in lines {
            let (key, value) = line.split_once('=').expect("报告行必须是 key=value");
            assert!(!key.is_empty());
            map.insert(String::from(key), String::from(value));
        }
        map
    }

    #[test]
    fn report_is_machine_parsable_and_describes_batch_population() {
        let result = super::super::run("report.parse", 64, || core::hint::black_box(1u32));
        let text = format!("{result}");
        let map = parse_report(&text);

        assert_eq!(map["method"], "batch");
        assert_eq!(map["unit"], clock_unit().as_str());
        assert_eq!(map["sample_unit"], "batch_total");
        assert_eq!(map["rounds"], "5");
        assert_eq!(map["batches_per_round"], "31");
        assert_eq!(map["samples"], "155");
        assert_eq!(map["status"], "ok");
        assert_eq!(map["baseline"], "paired_null");
        assert!(map["calibration_retries"].parse::<u64>().is_ok());
        assert!(map["resolution_limited"] == "yes" || map["resolution_limited"] == "no");
        assert!(map["below_floor_batches"].parse::<u64>().is_ok());
        assert!(map.contains_key("baseline_paired_diff_median"));
        for round in 0..5 {
            assert!(map.contains_key(&format!("round_{round}_median")));
        }

        let ops = map["operations_per_batch"].parse::<u64>().unwrap();
        let iterations = map["iterations"].parse::<u64>().unwrap();
        assert_eq!(iterations, ops * 155, "iterations = K * samples");
        let min = map["min"].parse::<u64>().unwrap();
        let median = map["median"].parse::<u64>().unwrap();
        let p95 = map["p95"].parse::<u64>().unwrap();
        let max = map["max"].parse::<u64>().unwrap();
        assert!(min <= median && median <= p95 && p95 <= max);
    }

    #[test]
    fn empty_result_still_renders_every_key() {
        let result = super::super::Bench::new("report.empty").finish();
        let text = format!("{result}");
        let map = parse_report(&text);
        assert_eq!(map["status"], "ok");
        assert_eq!(map["samples"], "0");
        assert_eq!(map["calibration_retries"], "0");
        assert_eq!(map["baseline_paired_diff_median"], "n/a");
    }
}
