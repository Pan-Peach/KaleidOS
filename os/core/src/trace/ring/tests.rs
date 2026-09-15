//! ring 的 host 测试：游标边界、覆盖缺口、锁外 visitor（可 emit / 查 stats）。
//!
//! 全局 ring 是单例，测试必须串行（与 Inspector 的读侧测试共用同一把锁）。

use super::*;
use crate::component::ComponentId;
use crate::component::ComponentState;
use crate::component::interface::{BindingId, InterfaceId};
use crate::handle::{RawHandle, ResourceKind};
use crate::task::TaskId;
use crate::trace::RejectReason;
use crate::trace::abi::TraceRecordAbi;
use alloc::vec::Vec;

use crate::trace::test_support::GUARD as TEST_LOCK;

fn switch(to: u32) -> TraceEvent {
    TraceEvent::TaskSwitch {
        from: None,
        to: TaskId::from_raw(to),
    }
}

fn collect(since: u64) -> Vec<u64> {
    let mut seqs = Vec::new();
    visit_since(since, |record| seqs.push(record.seq));
    seqs
}

/// 12 种事件的样本各一个（逐位开关测试用）。
fn all_events() -> [TraceEvent; 12] {
    let component = ComponentId::from_raw(3);
    let task = TaskId::from_raw(9);
    [
        switch(1),
        TraceEvent::PolicyProposal { component, task },
        TraceEvent::PolicyAccepted { component, task },
        TraceEvent::PolicyRejected {
            component,
            reason: RejectReason::NotRunnable,
        },
        TraceEvent::ComponentState {
            component,
            from: None,
            to: ComponentState::Declared,
        },
        TraceEvent::ResourceGrant {
            component,
            kind: ResourceKind::Mmio,
            handle: RawHandle::from_raw(1),
        },
        TraceEvent::ResourceRevoke {
            component,
            kind: ResourceKind::Dma,
            handle: RawHandle::from_raw(2),
        },
        TraceEvent::InterfaceBind {
            consumer: None,
            provider: component,
            interface: InterfaceId::from_raw(1),
        },
        TraceEvent::InterfaceRefresh {
            binding: BindingId::from_raw(1),
            generation: 2,
        },
        TraceEvent::IrqEnter { irq: 5 },
        TraceEvent::IrqDispatch {
            irq: 5,
            component: Some(component),
        },
        TraceEvent::IrqAck { irq: 5 },
    ]
}

#[test]
fn seq_is_monotonic_starting_at_one() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    for n in 1..=3 {
        emit(switch(n));
    }
    assert_eq!(collect(0), [1, 2, 3]);
    assert_eq!(next_seq(), 4);
}

#[test]
fn visit_since_skips_older_records() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    for n in 1..=5 {
        emit(switch(n));
    }
    assert_eq!(collect(3), [3, 4, 5]);
    assert!(collect(6).is_empty());
}

/// 游标边界：空 ring、精确 `since`、早于最旧、`since == next_seq`、越过终点。
#[test]
fn read_one_cursor_boundaries() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    // 空 ring：任何 since（包括等于 next_seq 的 1）都读不到。
    assert_eq!(read_one(0), None);
    assert_eq!(read_one(1), None);
    assert_eq!(read_one(99), None);

    for n in 1..=3 {
        emit(switch(n));
    }
    // 精确命中 since。
    assert_eq!(read_one(2).map(|record| record.seq), Some(2));
    // since 早于最旧（最旧 = seq 1）：从最旧开始。
    assert_eq!(read_one(0).map(|record| record.seq), Some(1));
    // since == next_seq / 越过终点：读空。
    assert_eq!(read_one(4), None);
    assert_eq!(read_one(99), None);
}

#[test]
fn overwrite_keeps_newest_and_counts_loss() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    let total = TRACE_CAPACITY as u32 + 2;
    for n in 0..total {
        emit(switch(n));
    }
    let seqs = collect(0);
    assert_eq!(seqs.len(), TRACE_CAPACITY);
    // 前两条（seq 1、2）被逐出保留区，最旧的存活记录是 seq 3。
    assert_eq!(seqs.first().copied(), Some(3));
    assert_eq!(seqs.last().copied(), Some(u64::from(total)));
    assert_eq!(
        stats(),
        TraceStats {
            oldest_seq: 3,
            next_seq: u64::from(total) + 1,
            overwritten_total: 2,
            enabled_mask: ENABLED_MASK_ALL,
        }
    );
}

/// 覆盖缺口：停在被逐出 seq 上的 reader 拿到最旧存活记录，缺口由
/// `returned_seq - requested_seq` 表达；遍历保持有界，不会卡在缺口上。
#[test]
fn overwrite_gap_is_visible_and_traversal_terminates() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    let total = TRACE_CAPACITY as u32 + 2;
    for n in 0..total {
        emit(switch(n));
    }
    let record = read_one(1).expect("被逐出的游标应拿到最旧存活记录");
    assert_eq!(record.seq, 3);
    assert_eq!(
        record.seq - 1,
        2,
        "reader 的真实缺口 = returned_seq - requested_seq"
    );
    // 起点已在覆盖区：collect 必须终止，并覆盖当前全部存活记录。
    let seqs = collect(1);
    assert_eq!(seqs.len(), TRACE_CAPACITY);
    assert_eq!(seqs.first().copied(), Some(3));
    assert_eq!(seqs.last().copied(), Some(u64::from(total)));
}

/// visitor 自己 emit：不得死锁，也不得把新事件卷入本次遍历（终点进入时已捕获）。
#[test]
fn visitor_may_emit_without_deadlock_or_unbounded_traversal() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    emit(switch(1));
    let mut seen = Vec::new();
    visit_since(0, |record| {
        seen.push(record.seq);
        if record.seq == 1 {
            emit(switch(2));
        }
    });
    assert_eq!(seen, [1], "进入时捕获的终点决定本次遍历范围");
    assert_eq!(next_seq(), 3);
    assert_eq!(collect(0), [1, 2], "新事件属于下一次遍历");
}

/// visitor 查 stats：同样必须在锁外可重入。
#[test]
fn visitor_may_query_stats_without_deadlock() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    emit(switch(1));
    let mut observed = None;
    let mut seqs = Vec::new();
    visit_since(0, |record| {
        seqs.push(record.seq);
        observed = Some(stats());
    });
    assert_eq!(seqs, [1]);
    let observed = observed.expect("visitor 必须能在锁外查 stats");
    assert_eq!(observed.oldest_seq, 1);
    assert_eq!(observed.next_seq, 2);
    assert_eq!(observed.enabled_mask, ENABLED_MASK_ALL);
}

#[test]
fn payload_survives_roundtrip() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    let event = TraceEvent::ResourceGrant {
        component: ComponentId::from_raw(9),
        kind: ResourceKind::Mmio,
        handle: RawHandle::from_raw(0x1234_5678_9abc_def0),
    };
    emit(event);
    let mut seen = Vec::new();
    visit_since(0, |record| seen.push(record.event));
    assert_eq!(seen, [event]);
}

/// runtime `clear` 只清记录、不回绕 seq：老 reader 的缺口仍然可计算。
#[test]
fn clear_keeps_seq_monotonic_for_existing_cursors() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    emit(switch(1));
    emit(switch(2));
    clear();
    assert!(collect(0).is_empty(), "clear 后没有留存记录");
    assert_eq!(next_seq(), 3, "runtime clear 不回绕 seq");
    emit(switch(3));
    assert_eq!(collect(1), [3], "旧游标 seq=1 拿到 seq=3（缺口 = 2）");
}

/// test-only 复位：记录、计数、`seq` 全部回到起点。
#[test]
fn reset_for_test_resets_seq_and_counters() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    emit(switch(1));
    reset_for_test();
    assert_eq!(next_seq(), 1);
    assert!(collect(0).is_empty());
    assert_eq!(
        stats(),
        TraceStats {
            oldest_seq: 1,
            next_seq: 1,
            overwritten_total: 0,
            enabled_mask: ENABLED_MASK_ALL,
        }
    );
}

/// 序号耗尽：停止记录，绝不回绕（回绕会让 seq 排序失去含义）。
#[test]
fn sequence_exhaustion_stops_recording_without_wrapping() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    RING.with(|cell| cell.borrow_mut().next_seq = u64::MAX - 1);
    emit(switch(1)); // seq = MAX-1，next_seq 推到 MAX
    emit(switch(2)); // checked_add(MAX) 溢出 → 停止记录
    assert_eq!(collect(0), [u64::MAX - 1], "耗尽后停止记录，绝不回绕");
    assert_eq!(next_seq(), u64::MAX);
}

// —— 运行时使能掩码 ——

/// 默认全开；类别掩码恰好覆盖全部事件位；每个 bit 与 ABI kind 一一对应。
#[test]
fn default_mask_is_all_on_and_matches_abi_kinds() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    assert_eq!(enabled_mask(), ENABLED_MASK_ALL, "默认必须全开");
    assert_eq!(
        MASK_TASK | MASK_POLICY | MASK_COMPONENT | MASK_RESOURCE | MASK_INTERFACE | MASK_IRQ,
        ENABLED_MASK_ALL as u32,
        "类别掩码必须恰好覆盖全部事件位（不多不少）"
    );
    for event in all_events() {
        let kind = TraceRecordAbi::from(&TraceRecord {
            seq: 0,
            timestamp: 0,
            event,
        })
        .kind;
        assert_eq!(
            event_bit(event),
            1 << (kind - 1),
            "bit 必须与 ABI kind 一一对应（kind={kind}）"
        );
    }
}

/// setter 返回旧掩码、写入值可读回；12 位之外的保留位写入前被钳掉。
#[test]
fn mask_round_trips_through_the_setter() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    let pattern = MASK_TASK | MASK_IRQ;
    assert_eq!(
        set_enabled_mask(pattern),
        ENABLED_MASK_ALL as u32,
        "setter 必须返回旧掩码"
    );
    assert_eq!(enabled_mask(), u64::from(pattern));
    assert_eq!(set_enabled_mask(u32::MAX), pattern, "返回被钳掉之前的旧值");
    assert_eq!(enabled_mask(), ENABLED_MASK_ALL, "保留位不得进入运行时掩码");
    reset_for_test();
}

/// 每个事件位都可单独关闭：关闭期间不落记录、不消耗 `seq`；打开后恢复。
#[test]
fn every_event_bit_is_individually_toggleable() {
    let _serial = TEST_LOCK.lock();
    for event in all_events() {
        reset_for_test();
        set_enabled_mask((ENABLED_MASK_ALL as u32) & !event_bit(event));

        let before = next_seq();
        emit(event);
        assert_eq!(next_seq(), before, "被过滤的事件不得消耗 seq");
        assert!(collect(0).is_empty(), "被过滤的事件不得进入 ring");

        set_enabled_mask(ENABLED_MASK_ALL as u32);
        emit(event);
        assert_eq!(next_seq(), before + 1, "使能后事件必须落盘");
        assert_eq!(collect(0), [before]);
    }
    reset_for_test();
}

/// 被禁用的事件**在读时钟之前**返回：不读钟、不进 ring、不消耗 `seq`。
#[test]
fn disabled_event_touches_neither_clock_nor_ring() {
    let _serial = TEST_LOCK.lock();
    reset_for_test();
    let event = TraceEvent::IrqAck { irq: 3 };
    set_enabled_mask((ENABLED_MASK_ALL as u32) & !event_bit(event));

    let clock_before = clock_reads();
    let seq_before = next_seq();
    emit(event);
    assert_eq!(clock_reads(), clock_before, "禁用事件必须在读时钟之前返回");
    assert_eq!(next_seq(), seq_before, "禁用事件不得消耗 seq");
    assert!(collect(0).is_empty(), "禁用事件不得进入 ring");

    // 对照：使能后同一事件确实读一次钟并落一条记录。
    set_enabled_mask(ENABLED_MASK_ALL as u32);
    emit(event);
    assert_eq!(clock_reads(), clock_before + 1);
    assert_eq!(collect(0), [seq_before]);
    reset_for_test();
}
