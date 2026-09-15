//! Trace 观察面的**只读**投影（组件侧）。
//!
//! 组件没有 trace 控制 authority（掩码由 Monitor 管理，组件只能经
//! `kcore_trace_stats` / `kcore_trace_read` 读），所以这里只做两件事：
//!
//! 1. 快照运行时状态（mask / capacity / seq 游标 / 逐出计数），让 `BENCH-ENV`
//!    如实说明被测数字里 trace 成本处于什么状态；
//! 2. 把 `TaskSwitch` 记录翻译成**值型** `(from, to)`（`ABSENT` → `None`），
//!    并据此做独立的任务交替验证（Core 事件流是调度真相，不是组件自己的计数）。
//!
//! 语义细节见 SDK `abi` 模块文档：`enabled_mask == 0` 在 ABI 上同时覆盖
//! "编译期 `CONFIG_TRACE=n`"与"运行时掩码全关"；被过滤的事件不记录、不消耗
//! `seq`；ring 覆盖时 reader 的真实缺口 = `record.seq - since`。

use kcomp_sdk::abi::{self, TraceRecordAbi};
use kcomp_sdk::abi::{ABSENT, KIND_TASK_SWITCH};

/// 独立验证的读记录上界（防御性；正常由 ring 边界自然终止）。
const MAX_TRACE_READS: usize = 1024;
/// 交替验证的往返数上限（数组上界；一次 16 次往返 = 32 条 TaskSwitch 事件）。
pub(crate) const VALIDATION_TRIPS: usize = 16;
/// TaskSwitch 的使能位（bit i ↔ ABI kind i+1）。
const TASK_SWITCH_MASK_BIT: u64 = 1;

/// Trace 子系统状态快照（只读一次 `kcore_trace_stats`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TraceState {
    pub capacity: u64,
    pub oldest_seq: u64,
    pub next_seq: u64,
    pub overwritten_total: u64,
    pub enabled_mask: u64,
}

/// 读取当前 trace 状态；export 失败返回 `None`（不猜）。
pub(crate) fn state() -> Option<TraceState> {
    let mut stats = abi::TraceStatsAbi {
        capacity: 0,
        oldest_seq: 0,
        next_seq: 0,
        overwritten_total: 0,
        enabled_mask: 0,
    };
    match unsafe { abi::kcore_trace_stats(&mut stats) } {
        0 => Some(TraceState {
            capacity: stats.capacity,
            oldest_seq: stats.oldest_seq,
            next_seq: stats.next_seq,
            overwritten_total: stats.overwritten_total,
            enabled_mask: stats.enabled_mask,
        }),
        _ => None,
    }
}

/// 是否已有事件真的被记录（"trace 被编译进来"的直接证据）。
///
/// 只读探测：从最旧存活 seq 读一条；成功 = 至少有记录。`false` 的两种原因
/// （编译期关掉 / 运行时全关）在 ABI 上同形，报告里如实说明。
pub(crate) fn has_records() -> bool {
    let Some(state) = state() else {
        return false;
    };
    let mut record = empty_record();
    let mut next = 0u64;
    unsafe { abi::kcore_trace_read(state.oldest_seq, &mut record, &mut next) == 0 }
}

/// 读下一条 `seq >= since` 的记录。成功 = `Some((record, next_seq))`；
/// `-ENOENT` / 其他失败 = `None`（读侧完成或不可用）。
pub(crate) fn read_since(since: u64) -> Option<(TraceRecordAbi, u64)> {
    let mut record = empty_record();
    let mut next = 0u64;
    match unsafe { abi::kcore_trace_read(since, &mut record, &mut next) } {
        0 => Some((record, next)),
        _ => None,
    }
}

fn empty_record() -> TraceRecordAbi {
    TraceRecordAbi {
        seq: 0,
        timestamp: 0,
        kind: 0,
        flags: 0,
        a: 0,
        b: 0,
        c: 0,
    }
}

/// `TaskSwitch` 记录的语义解包：`a` = from（[`ABSENT`] = 从锚点进入），
/// `b` = to。非 TaskSwitch / payload 超 `u32` 范围 → `None`（不猜）。
pub(crate) fn task_switch(record: &TraceRecordAbi) -> Option<(Option<u32>, u32)> {
    if record.kind != KIND_TASK_SWITCH {
        return None;
    }
    let from = if record.a == ABSENT {
        None
    } else {
        Some(u32::try_from(record.a).ok()?)
    };
    let to = u32::try_from(record.b).ok()?;
    Some((from, to))
}

/// 纯逻辑：Core `TaskSwitch` 序列必须是 A→B、B→A 交替，恰好 `trips * 2` 条。
/// （host-testable；`from` 必须正好是 A/B——出现锚点或其它任务即失败。）
pub(crate) fn alternation_ok(
    switches: &[(Option<u32>, u32)],
    trips: usize,
    a: u32,
    b: u32,
) -> bool {
    if switches.len() != trips * 2 {
        return false;
    }
    let mut index = 0usize;
    while index < switches.len() {
        let (from, to) = switches[index];
        let (expected_from, expected_to) = if index.is_multiple_of(2) {
            (a, b)
        } else {
            (b, a)
        };
        if from != Some(expected_from) || to != expected_to {
            return false;
        }
        index += 1;
    }
    true
}

/// 任务交替的**独立正确性验证**（在计时之外调用）。
///
/// `trip` 执行一次 A→B→A 往返，连续执行 `trips` 次；随后按 Core 的
/// `TaskSwitch` 事件流验证 `trips * 2` 次切换严格 A→B / B→A 交替。
///
/// 只在 TaskSwitch 使能（mask bit 0）时执行；否则如实报 `masked`（编译期
/// `CONFIG_TRACE=n` 与运行时全关在 ABI 上同形）。ring 覆盖（逐出计数变化）
/// → `lost_records`，超出预期切换数 / pattern 不符 → `mismatch`，绝不把缺口
/// 当成功。
pub(crate) fn validate_task_alternation(
    a: u32,
    b: u32,
    trips: usize,
    trip: &mut impl FnMut(),
) -> &'static str {
    let Some(before) = state() else {
        return "read_failed";
    };
    if before.enabled_mask & TASK_SWITCH_MASK_BIT == 0 {
        return "masked";
    }
    if trips == 0 || trips > VALIDATION_TRIPS {
        return "invalid_trips";
    }

    let mut trips_done = 0usize;
    while trips_done < trips {
        trip();
        trips_done += 1;
    }

    let Some(after) = state() else {
        return "read_failed";
    };
    if after.overwritten_total != before.overwritten_total {
        return "lost_records";
    }

    let mut switches = [(None, 0u32); VALIDATION_TRIPS * 2];
    let mut found = 0usize;
    let mut cursor = before.next_seq;
    let mut reads = 0usize;
    while reads < MAX_TRACE_READS {
        let Some((record, next)) = read_since(cursor) else {
            break;
        };
        if let Some(pair) = task_switch(&record) {
            if found >= switches.len() {
                // 超出预期数量的切换 = pattern 不可能交替（单 CPU 上只有 A/B）。
                return "mismatch";
            }
            switches[found] = pair;
            found += 1;
        }
        cursor = next;
        reads += 1;
    }
    if alternation_ok(&switches[..found], trips, a, b) {
        "ok"
    } else {
        "mismatch"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u32, a: u64, b: u64) -> TraceRecordAbi {
        TraceRecordAbi {
            seq: 1,
            timestamp: 0,
            kind,
            flags: 0,
            a,
            b,
            c: 0,
        }
    }

    #[test]
    fn task_switch_unpacks_absent_from_as_none() {
        assert_eq!(
            task_switch(&record(KIND_TASK_SWITCH, ABSENT, 7)),
            Some((None, 7))
        );
    }

    #[test]
    fn task_switch_unpacks_real_ids() {
        assert_eq!(
            task_switch(&record(KIND_TASK_SWITCH, 3, 5)),
            Some((Some(3), 5))
        );
    }

    #[test]
    fn task_switch_ignores_other_kinds() {
        assert_eq!(task_switch(&record(KIND_TASK_SWITCH + 1, 3, 5)), None);
    }

    #[test]
    fn task_switch_rejects_out_of_range_payload() {
        assert_eq!(
            task_switch(&record(KIND_TASK_SWITCH, u64::from(u32::MAX) + 1, 5)),
            None
        );
        assert_eq!(
            task_switch(&record(KIND_TASK_SWITCH, 3, u64::from(u32::MAX) + 1)),
            None
        );
    }

    #[test]
    fn alternation_accepts_a_b_a_b() {
        let switches = [(Some(3), 5), (Some(5), 3), (Some(3), 5), (Some(5), 3)];
        assert!(alternation_ok(&switches, 2, 3, 5));
    }

    #[test]
    fn alternation_rejects_wrong_length() {
        let switches = [(Some(3), 5), (Some(5), 3), (Some(3), 5)];
        assert!(!alternation_ok(&switches, 2, 3, 5));
    }

    #[test]
    fn alternation_rejects_repeated_task_switches() {
        let switches = [(Some(3), 5), (Some(3), 5)];
        assert!(!alternation_ok(&switches, 1, 3, 5));
    }

    #[test]
    fn alternation_rejects_anchor_or_foreign_participants() {
        let with_anchor = [(None, 5), (Some(5), 3)];
        assert!(!alternation_ok(&with_anchor, 1, 3, 5));
        let with_foreign = [(Some(9), 5), (Some(5), 9)];
        assert!(!alternation_ok(&with_foreign, 1, 3, 5));
    }

    #[test]
    fn alternation_rejects_wrong_direction() {
        let switches = [(Some(5), 3), (Some(3), 5)];
        assert!(!alternation_ok(&switches, 1, 3, 5));
    }
}
