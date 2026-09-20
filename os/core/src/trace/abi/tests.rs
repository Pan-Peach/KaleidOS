//! trace ABI 编码的 host 锚定测试：稳定标签 / 哨兵 / 枚举码 / 布局指纹。

use super::*;
use crate::component::ComponentId;
use crate::resource::ResourceKind;
use crate::task::TaskId;

fn record(seq: u64, event: TraceEvent) -> TraceRecord {
    TraceRecord {
        seq,
        timestamp: 7,
        event,
    }
}

fn encode(event: TraceEvent) -> TraceRecordAbi {
    TraceRecordAbi::from(&record(42, event))
}

/// 每个事件都有**唯一**的稳定标签，且 `flags` 恒为 0。
#[test]
fn every_event_has_a_distinct_stable_tag() {
    let component = ComponentId::from_raw(3);
    let task = TaskId::from_raw(9);
    let events = [
        TraceEvent::TaskSwitch {
            from: Some(task),
            to: task,
        },
        TraceEvent::PolicyProposal { component, task },
        TraceEvent::PolicyAccepted { component, task },
        TraceEvent::PolicyRejected {
            component,
            reason: RejectReason::NotRunnable,
        },
        TraceEvent::ComponentState {
            component,
            from: Some(ComponentState::Ready),
            to: ComponentState::Failed,
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
        TraceEvent::InterfaceBind {
            consumer: Some(component),
            provider: component,
            interface: crate::component::interface::InterfaceId::from_raw(1),
        },
        TraceEvent::InterfaceRefresh {
            binding: crate::component::interface::BindingId::from_raw(1),
            generation: 2,
        },
        TraceEvent::IrqEnter { irq: 5 },
        TraceEvent::IrqDispatch {
            irq: 5,
            component: Some(component),
        },
        TraceEvent::IrqAck { irq: 5 },
    ];
    let tags: alloc::vec::Vec<u32> = events.iter().map(|event| encode(*event).kind).collect();
    assert_eq!(tags, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    for event in events {
        let abi = encode(event);
        assert_eq!(abi.flags, 0, "flags 必须为 0");
        assert_eq!(abi.seq, 42, "seq 必须原样透传");
        assert_eq!(abi.timestamp, 7, "timestamp 必须原样透传");
    }
}

/// 缺省字段用 `ABSENT` 显式表达，不与"值 0"混淆。
#[test]
fn absent_fields_use_the_sentinel() {
    let abi = encode(TraceEvent::TaskSwitch {
        from: None,
        to: TaskId::from_raw(0),
    });
    assert_eq!(abi.a, ABSENT, "没有前驱任务必须写成 ABSENT 而不是 0");
    assert_eq!(abi.b, 0, "to = TaskId(0) 是真实值，不能和 ABSENT 混淆");

    let abi = encode(TraceEvent::ComponentState {
        component: ComponentId::from_raw(0),
        from: None,
        to: ComponentState::Declared,
    });
    assert_eq!(abi.b, ABSENT, "出生（无前态）必须是 ABSENT");
    assert_eq!(abi.c, 0, "Declared = 0 是真实状态码");
}

/// 枚举值映射成稳定数字码（不是 Rust 的 discriminant 布局）。
#[test]
fn enum_values_map_to_stable_codes() {
    let abi = encode(TraceEvent::ResourceGrant {
        component: ComponentId::from_raw(1),
        kind: ResourceKind::Irq,
        id: 0xdead,
    });
    assert_eq!(abi.b, 1, "ResourceKind::Irq = 1");
    assert_eq!(abi.c, 0xdead, "raw handle 原样透传");

    let abi = encode(TraceEvent::ComponentState {
        component: ComponentId::from_raw(1),
        from: Some(ComponentState::Starting),
        to: ComponentState::Ready,
    });
    assert_eq!(abi.b, 2, "Starting = 2");
    assert_eq!(abi.c, 3, "Ready = 3");
}

/// `TraceStatsAbi` 布局锚定（kcomp-sdk 侧有同一断言，改了字段必须双侧同步）。
#[test]
fn stats_abi_layout_is_anchored() {
    assert_eq!(core::mem::size_of::<TraceStatsAbi>(), 40);
    assert_eq!(core::mem::align_of::<TraceStatsAbi>(), 8);
}

/// `TraceStatsAbi` 逐字段镜像 [`TraceStats`]（`capacity` 来自编译期常量）。
#[test]
fn stats_abi_mirrors_stats_fields() {
    let abi = TraceStatsAbi::from(&TraceStats {
        oldest_seq: 3,
        next_seq: 9,
        overwritten_total: 2,
        enabled_mask: 0xAA,
    });
    assert_eq!(abi.capacity, capacity() as u64);
    assert_eq!(abi.oldest_seq, 3);
    assert_eq!(abi.next_seq, 9);
    assert_eq!(abi.overwritten_total, 2);
    assert_eq!(abi.enabled_mask, 0xAA);
}
