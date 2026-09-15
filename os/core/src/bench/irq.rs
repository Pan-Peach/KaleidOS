//! IRQ 服务路径延迟：从 trace 记录里抽取 `IrqEnter → IrqDispatch → IrqAck` 三元组。
//!
//! 纯函数、host 可测。规则只有一条：**不猜时间**。
//! - 新的 `IrqEnter` 必须清掉该线旧的 dispatch 状态（旧三元组作废）；
//! - 序列出现缺口（ring 覆盖过记录）时，半截三元组可能缺腿 → 整体丢弃；
//! - 时间戳倒退（`t_ack < t_dispatch` 等）→ 丢弃，不用饱和减法把负差值伪装成 0。

use crate::trace::{TraceEvent, TraceRecord};
use alloc::vec::Vec;

/// 一次外部中断的服务路径耗时（单位同 [`crate::bench::clock_unit`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqLatency {
    pub irq: u32,
    /// trap 进入 → 路由到组件。
    pub enter_to_dispatch: u64,
    /// 路由 → ack（控制器 complete）。
    pub dispatch_to_ack: u64,
    /// 整条服务路径（enter → ack）。
    pub total: u64,
}

/// 每个 IRQ 线的三元组状态机：0 = idle，1 = 已 enter，2 = 已 dispatch。
const IDLE: u8 = 0;
const ENTERED: u8 = 1;
const DISPATCHED: u8 = 2;

/// trace 事件类别（与状态分开编码，避免混淆）。
const KIND_ENTER: u8 = 1;
const KIND_DISPATCH: u8 = 2;
const KIND_ACK: u8 = 3;

/// 从 trace 记录里抽出 IRQ 服务路径（**纯函数**，host 可测）。
///
/// 只认同一中断号上按序出现的 `IrqEnter → IrqDispatch → IrqAck` 三元组；
/// 不完整、乱序或跨序列缺口（ring 被覆盖）就丢弃那一组，**不猜时间**。
/// IRQ 号 >= 256 的线不参与统计（如实丢弃，不分配无界表）。
pub fn irq_latency(records: &[TraceRecord]) -> Vec<IrqLatency> {
    const SLOTS: usize = 256;
    let mut state = [IDLE; SLOTS];
    let mut enter_at = [0u64; SLOTS];
    let mut dispatch_at = [0u64; SLOTS];
    let mut out = Vec::new();
    let mut previous_seq: Option<u64> = None;

    for record in records {
        // 序列缺口 = 中间有记录被 ring 覆盖：任何在途三元组都可能缺腿，全部作废。
        if let Some(previous) = previous_seq
            && record.seq != previous.wrapping_add(1)
        {
            state = [IDLE; SLOTS];
        }
        previous_seq = Some(record.seq);

        let (irq, kind) = match record.event {
            TraceEvent::IrqEnter { irq } => (irq, KIND_ENTER),
            TraceEvent::IrqDispatch { irq, .. } => (irq, KIND_DISPATCH),
            TraceEvent::IrqAck { irq } => (irq, KIND_ACK),
            _ => continue,
        };
        let Ok(slot) = usize::try_from(irq) else {
            continue;
        };
        if slot >= SLOTS {
            continue;
        }

        match kind {
            KIND_ENTER => {
                // 新 enter 清掉旧 dispatch：绝不让上一轮的中间态配到这一轮的 ack。
                state[slot] = ENTERED;
                enter_at[slot] = record.timestamp;
            }
            KIND_DISPATCH => {
                if state[slot] == ENTERED && record.timestamp >= enter_at[slot] {
                    state[slot] = DISPATCHED;
                    dispatch_at[slot] = record.timestamp;
                } else {
                    state[slot] = IDLE;
                }
            }
            _ => {
                if state[slot] == DISPATCHED && record.timestamp >= dispatch_at[slot] {
                    out.push(IrqLatency {
                        irq,
                        enter_to_dispatch: dispatch_at[slot] - enter_at[slot],
                        dispatch_to_ack: record.timestamp - dispatch_at[slot],
                        total: record.timestamp - enter_at[slot],
                    });
                }
                state[slot] = IDLE;
            }
        }
    }
    out
}

/// 读当前 trace ring 并算出 IRQ 服务路径。
///
/// 目标端由 selftest / monitor 调用（真实 QEMU IRQ 路径）；host 上因为
/// `context_switch` / 中断控制器都是 Fake，拿不到真实数据，但计算逻辑本身
/// 已被 [`irq_latency`] 的 host test 覆盖。
pub fn collect_irq_latency() -> Vec<IrqLatency> {
    let mut records = Vec::new();
    crate::trace::visit_since(0, |record| records.push(*record));
    irq_latency(&records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: u64, timestamp: u64, event: TraceEvent) -> TraceRecord {
        TraceRecord {
            seq,
            timestamp,
            event,
        }
    }

    fn enter(irq: u32) -> TraceEvent {
        TraceEvent::IrqEnter { irq }
    }

    fn dispatch(irq: u32) -> TraceEvent {
        TraceEvent::IrqDispatch {
            irq,
            component: None,
        }
    }

    fn ack(irq: u32) -> TraceEvent {
        TraceEvent::IrqAck { irq }
    }

    #[test]
    fn irq_latency_pairs_enter_dispatch_ack() {
        let records = [
            record(1, 100, enter(9)),
            record(2, 140, dispatch(9)),
            record(3, 175, ack(9)),
        ];
        assert_eq!(
            irq_latency(&records),
            [IrqLatency {
                irq: 9,
                enter_to_dispatch: 40,
                dispatch_to_ack: 35,
                total: 75,
            }]
        );
    }

    #[test]
    fn incomplete_irq_triples_are_skipped_not_guessed() {
        // 只有 enter + ack（缺 dispatch）：不产出，也不把 ack 当成 dispatch。
        let records = [record(1, 10, enter(3)), record(2, 90, ack(3))];
        assert!(irq_latency(&records).is_empty());
    }

    #[test]
    fn irq_latency_keeps_lines_independent() {
        let records = [
            record(1, 0, enter(1)),
            record(2, 5, enter(2)),
            record(3, 10, dispatch(2)),
            record(4, 30, ack(2)),
            record(5, 40, dispatch(1)),
            record(6, 60, ack(1)),
        ];
        let latencies = irq_latency(&records);
        assert_eq!(latencies.len(), 2);
        assert_eq!(latencies[0].irq, 2, "按 ack 顺序产出");
        assert_eq!(latencies[0].total, 25);
        assert_eq!(latencies[1].irq, 1);
        assert_eq!(latencies[1].total, 60);
    }

    #[test]
    fn a_new_enter_clears_stale_dispatch_state() {
        // 旧 bug：第二个 enter 会配上第一个 dispatch，enter_to_dispatch 被 saturating
        // 减法伪装成 0。正确行为：旧 dispatch 作废，只产出第二轮三元组。
        let records = [
            record(1, 0, enter(5)),
            record(2, 10, dispatch(5)),
            record(3, 20, enter(5)),
            record(4, 30, dispatch(5)),
            record(5, 40, ack(5)),
        ];
        let latencies = irq_latency(&records);
        assert_eq!(latencies.len(), 1);
        assert_eq!(latencies[0].enter_to_dispatch, 10);
        assert_eq!(latencies[0].dispatch_to_ack, 10);
        assert_eq!(latencies[0].total, 20);
    }

    #[test]
    fn reversed_timestamps_discard_the_interval_instead_of_saturating() {
        let records = [
            record(1, 100, enter(9)),
            record(2, 90, dispatch(9)),
            record(3, 80, ack(9)),
        ];
        assert!(
            irq_latency(&records).is_empty(),
            "时间戳倒退必须丢弃，不能 saturating 成 0"
        );
    }

    #[test]
    fn dispatch_before_enter_is_discarded() {
        let records = [
            record(1, 10, dispatch(4)),
            record(2, 20, enter(4)),
            record(3, 30, dispatch(4)),
            record(4, 40, ack(4)),
        ];
        let latencies = irq_latency(&records);
        assert_eq!(latencies.len(), 1, "第一个无主 dispatch 不能配对");
        assert_eq!(latencies[0].total, 20);
    }

    #[test]
    fn sequence_gap_discards_partial_triples() {
        // seq 2 缺失：enter 与后面的 dispatch/ack 之间可能还丢过记录。
        let records = [
            record(1, 0, enter(7)),
            record(3, 50, dispatch(7)),
            record(4, 90, ack(7)),
        ];
        assert!(
            irq_latency(&records).is_empty(),
            "序列缺口必须丢弃半截三元组"
        );
    }

    #[test]
    fn sequence_gap_only_invalidates_the_partial_interval() {
        // 缺口后重新开始的三元组（seq 连续）仍然有效。
        let records = [
            record(1, 0, enter(7)),
            record(3, 10, enter(7)),
            record(4, 20, dispatch(7)),
            record(5, 30, ack(7)),
        ];
        let latencies = irq_latency(&records);
        assert_eq!(latencies.len(), 1);
        assert_eq!(latencies[0].total, 20);
    }

    #[test]
    fn duplicate_dispatch_discards_the_triple() {
        let records = [
            record(1, 0, enter(6)),
            record(2, 10, dispatch(6)),
            record(3, 15, dispatch(6)),
            record(4, 30, ack(6)),
        ];
        assert!(irq_latency(&records).is_empty());
    }
}
