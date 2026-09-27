//! ring 的 host 测试：游标边界、覆盖缺口、锁外 visitor（可 emit / 查 stats）。
//!
//! 全局 ring 是单例，测试必须串行。

use super::*;
use crate::component::ComponentId;
use crate::component::ComponentState;
use crate::component::endpoint::{EndpointId, Mechanism};
use crate::resource::ResourceKind;
use crate::task::TaskId;
use crate::trace::RejectReason;
use crate::trace::abi::TraceRecordAbi;
use alloc::collections::VecDeque;
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

/// 11 种事件的样本各一个（逐位开关测试用）。
fn all_events() -> [TraceEvent; 11] {
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
            kind: ResourceKind::Device,
            id: 1,
        },
        TraceEvent::ResourceRevoke {
            component,
            kind: ResourceKind::Dma,
            id: 2,
        },
        TraceEvent::EndpointBind {
            endpoint: EndpointId::from_raw(1),
            provider: component,
            mechanism: Mechanism::Direct,
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
        kind: ResourceKind::Device,
        id: 0x1234_5678_9abc_def0,
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
        MASK_TASK | MASK_POLICY | MASK_COMPONENT | MASK_RESOURCE | MASK_ENDPOINT | MASK_IRQ,
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

/// setter 返回旧掩码、写入值可读回；11 位之外的保留位写入前被钳掉。
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

// —— Property tests：随机 emit / visit / clear 序列上的 ring 记账不变量 ——
//
// 把 ring 模块文档承诺的语义（docs/development/testing.md §4）编码成影子模型，逐操作核对：
//   1. `seq` 由 Core 分配、从 1 起严格单调（`next_seq == 1 + 成功 emit 次数`）。
//   2. `overwritten_total` 精确 == `max(0, emitted - capacity)`，无静默丢失。
//   3. ring 绝不超过 `capacity()` 条，且保留的总是**最新**记录。
//   4. `visit_since(cursor)` 严格递增、跳过比 cursor 更旧的记录，
//      缺口 = `record.seq - requested_seq`。
//   5. `read_one` 游标边界：一次推进一条，越过终点报告结束而非伪造记录。
//   6. 序号耗尽停止记录、绝不回绕（bounded 序列无法自然触达，见
//      `random_emits_near_exhaustion_never_wrap`）。
//
// **host 局限**：`#[cfg(test)]` 下生产 `Mutex<TraceRing>` 被替换为 thread_local
// （见 ring.rs 顶部注释），跨线程 / SMP / 中断重入行为**不可观测**，本文件不做
// 任何并发断言；那部分由 QEMU / 真机承担。

use proptest::prelude::*;

/// 一条随机 ring 操作。
#[derive(Debug, Clone, Copy)]
enum RingOp {
    /// 从 `all_events()` 里选 `which`，发射 `count` 条。
    Emit { which: u8, count: u16 },
    /// 从"当前 `next_seq` 往回 `back` 条"的位置读一条（覆盖精确命中 / 逐出缺口 /
    /// 越过终点 / 早于最旧）。
    ReadOne { back: u32 },
    /// 同上的起点做一次有界遍历。
    Visit { back: u32 },
    /// runtime `clear()`：清记录与逐出计数，但保留 `seq`。
    Clear,
}

/// 随机 op 序列（长度有界；单次 Emit 可批量，故能跨过 `capacity()`）。
fn op_seq() -> impl Strategy<Value = Vec<RingOp>> {
    proptest::collection::vec(op_kind_strategy(), 1..=200)
}

fn op_kind_strategy() -> impl Strategy<Value = RingOp> {
    // 回看窗口略大于容量：既命中留存区，也落进被逐出的缺口 / 越过终点。
    let back = 0u32..=(capacity() as u32 + 8);
    prop_oneof![
        3 => (any::<u8>(), 1u16..=64u16)
            .prop_map(|(which, count)| RingOp::Emit { which, count }),
        1 => back.clone().prop_map(|back| RingOp::ReadOne { back }),
        1 => back.prop_map(|back| RingOp::Visit { back }),
        1 => Just(RingOp::Clear),
    ]
}

/// 影子模型：ring 应满足的记账真值（与实现同构，独立推导）。
struct Model {
    /// 下一条记录将拿到的 `seq`（从 1 起）。
    next_seq: u64,
    /// 成功 emit 的总数（`next_seq == 1 + total_emits`；`clear` 不重置）。
    total_emits: u64,
    /// 自上次 `clear` 以来成功 emit 数（决定 `overwritten_total`）。
    emitted_since_clear: u64,
    /// 当前留存记录的 `seq`（旧 → 新）；长度 <= `capacity()`。
    retained: VecDeque<u64>,
}

impl Model {
    fn new() -> Self {
        Self {
            next_seq: 1,
            total_emits: 0,
            emitted_since_clear: 0,
            retained: VecDeque::new(),
        }
    }

    /// 最旧存活记录的 `seq`；无记录时 == `next_seq`。
    fn oldest(&self) -> u64 {
        self.retained.front().copied().unwrap_or(self.next_seq)
    }

    /// 一次成功 emit：分配 `seq`，必要时逐出最旧记录。
    fn record(&mut self, seq: u64) {
        self.retained.push_back(seq);
        if self.retained.len() > capacity() {
            self.retained.pop_front();
        }
        self.total_emits += 1;
        self.emitted_since_clear += 1;
        self.next_seq = seq + 1;
    }
}

/// `read_one(since)` 的模型期望：`seq >= since` 的最旧存活记录；越过终点读空。
fn model_read_one(model: &Model, since: u64) -> Option<u64> {
    let oldest = model.retained.front().copied()?;
    let wanted = since.max(oldest);
    (wanted < model.next_seq).then_some(wanted)
}

/// 断言 ring 的完整快照（stats + 留存内容）与模型一致。
fn assert_matches_model(model: &Model) {
    let snapshot = stats();
    // 1. seq 从 1 起、每次成功 emit 恰好 +1（`clear` 不回绕）。
    assert_eq!(
        model.next_seq,
        1 + model.total_emits,
        "seq 必须从 1 起严格单调"
    );
    assert_eq!(snapshot.next_seq, model.next_seq, "next_seq 必须等于模型");
    // 2. overwritten_total 精确 == max(0, emitted - capacity)。
    assert_eq!(
        snapshot.overwritten_total,
        model.emitted_since_clear.saturating_sub(capacity() as u64),
        "overwritten_total 必须精确 == max(0, emitted - capacity)"
    );
    // 3. 绝不超容量，且留存的就是最新记录。
    assert!(
        model.retained.len() <= capacity(),
        "ring 留存数 {} 超过容量 {}",
        model.retained.len(),
        capacity()
    );
    assert_eq!(
        snapshot.oldest_seq,
        model.oldest(),
        "oldest_seq 必须等于模型"
    );
    assert_eq!(snapshot.enabled_mask, ENABLED_MASK_ALL);
    let expected: Vec<u64> = model.retained.iter().copied().collect();
    assert_eq!(
        collect(0),
        expected,
        "ring 必须保留最新的记录（旧记录被逐出）"
    );
}

/// 不变式 5：`read_one` 游标边界（精确命中 / 逐出缺口 / 越过终点）。
fn check_read_one(model: &Model, back: u32) {
    let since = model.next_seq.saturating_sub(u64::from(back));
    let record = read_one(since);
    assert_eq!(
        record.map(|r| r.seq),
        model_read_one(model, since),
        "read_one(since={since}) 边界不符"
    );
    if let Some(record) = record {
        assert!(record.seq < model.next_seq, "不得返回尚未分配的 seq");
        // 游标推进一条：下一条要么是相邻留存记录，要么报告结束。
        let next = record.seq.saturating_add(1);
        assert_eq!(
            read_one(next).map(|r| r.seq),
            model_read_one(model, next),
            "read_one 必须一次推进一条"
        );
    }
    // 越过终点 / 最大游标：报告结束，绝不伪造记录。
    assert_eq!(read_one(model.next_seq), None, "since == next_seq 必须读空");
    assert_eq!(read_one(u64::MAX), None, "游标耗尽必须读空");
}

/// 不变式 4：`visit_since` 跳过旧记录、严格递增、缺口可计算。
fn check_visit(model: &Model, back: u32) {
    let since = model.next_seq.saturating_sub(u64::from(back));
    let seqs = collect(since);
    let expected: Vec<u64> = model
        .retained
        .iter()
        .copied()
        .filter(|seq| *seq >= since)
        .collect();
    assert_eq!(seqs, expected, "visit_since(since={since}) 结果不符");
    // 非递减（实现里是严格递增）且不返回比 cursor 更旧的记录。
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "visit 必须按 seq 严格递增"
    );
    assert!(
        seqs.iter().all(|seq| *seq >= since),
        "不得返回比 cursor 更旧的记录"
    );
    if let Some(&first) = seqs.first() {
        // 缺口 = returned_seq - requested_seq；被逐出时从最旧存活记录起步。
        assert_eq!(
            first,
            since.max(model.oldest()),
            "遍历起点必须是游标或最旧存活"
        );
    }
}

/// 施加一次批量 emit，模型与实现同构（耗尽时停止，见不变式 6）。
fn apply_emit(model: &mut Model, which: u8, count: u16, events: &[TraceEvent; 11]) {
    for _ in 0..count {
        // bounded 序列不可达 `u64::MAX`；守卫只为与实现同构。
        if model.next_seq.checked_add(1).is_none() {
            break;
        }
        let seq = model.next_seq;
        emit(events[usize::from(which) % events.len()]);
        model.record(seq);
    }
}

proptest! {
    /// 不变式 1–5：随机 emit / read_one / visit_since / clear 序列上的记账。
    #[test]
    fn random_ring_ops_preserve_accounting_invariants(ops in op_seq()) {
        let _serial = TEST_LOCK.lock();

        // Given: 干净 ring（seq 回到 1、无记录、掩码全开）。
        reset_for_test();
        let mut model = Model::new();
        let events = all_events();
        assert_matches_model(&model);

        // When: 依次施加随机操作。
        for op in ops {
            match op {
                RingOp::Emit { which, count } => apply_emit(&mut model, which, count, &events),
                RingOp::Clear => {
                    clear();
                    // runtime clear 只清记录与逐出计数，`seq` 不回绕。
                    model.retained.clear();
                    model.emitted_since_clear = 0;
                }
                RingOp::ReadOne { back } => check_read_one(&model, back),
                RingOp::Visit { back } => check_visit(&model, back),
            }

            // Then: 每一步之后记账与留存内容都必须与模型一致。
            assert_matches_model(&model);
        }
    }

    /// 不变式 6：序号耗尽可能无法在 bounded 随机序列里自然出现，故把 `next_seq`
    /// 直接推到 `u64::MAX - back` 再随机发射：耗尽前每次 +1，耗尽后停止记录、
    /// 绝不回绕。
    #[test]
    fn random_emits_near_exhaustion_never_wrap(back in 0u16..=8, count in 1u16..=32) {
        let _serial = TEST_LOCK.lock();

        // Given: 序号被推到耗尽边界附近。
        reset_for_test();
        let start = u64::MAX - u64::from(back);
        RING.with(|cell| cell.borrow_mut().next_seq = start);

        // When: 再发射 `count` 次（`back` 次成功，其余在耗尽后停止）。
        let events = all_events();
        let successful = u64::from(count).min(u64::from(back));
        let mut expected = Vec::new();
        for step in 0..count {
            let before = next_seq();
            emit(events[usize::from(step) % events.len()]);
            if u64::from(step) < successful {
                expected.push(before);
                assert_eq!(next_seq(), before + 1, "耗尽前每次 emit 恰好 +1");
            } else {
                assert_eq!(next_seq(), before, "耗尽后必须停止记录，绝不回绕");
            }
        }

        // Then: 留存的就是耗尽前的记录，seq 严格递增且从不分配 u64::MAX。
        assert_eq!(
            next_seq(),
            start + successful,
            "next_seq 冻结在 u64::MAX 以内"
        );
        assert_eq!(collect(0), expected, "耗尽后不得再落记录");
        assert!(expected.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            expected.iter().all(|seq| *seq < u64::MAX),
            "u64::MAX 本身永不分配给记录"
        );
    }
}
