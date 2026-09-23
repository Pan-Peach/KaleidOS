//! 组件侧的**稳定 trace 记录编码**（组件 ABI 的地基）。
//!
//! 约束（AGENTS.md）：跨 ABI 边界**禁止** Rust enum layout、mangled symbol、
//! trait object、指针。所以这里是一个 `#[repr(C)]` 的**固定形状**记录：
//! 稳定的 `kind` 标签 + 三个 64 位 payload 词；缺省值用 [`ABSENT`] 显式表达，
//! 而不是依赖 `Option` 的 niche / 布局（那是 Rust 的实现细节，不是契约）。
//!
//! `kind` 取值与 payload 分配是 **ABI 契约的一部分**：只增不改。本阶段不维护
//! 兼容，契约变了就原地替换（不保留旧编号）：删除事件后**重新编号**，编号保持
//! 连续——`enabled_mask` 的 `bit i ↔ kind i+1` 契约不允许空洞。
//!
//! # payload 分配表
//!
//! ```text
//! kind                         a                  b                    c
//! 1  TASK_SWITCH               from(TaskId|ABSENT) to(TaskId)          -
//! 2  POLICY_PROPOSAL           component          task                 -
//! 3  POLICY_ACCEPTED           component          task                 -
//! 4  POLICY_REJECTED           component          reason               -
//! 5  COMPONENT_STATE           component          from(State|ABSENT)   to(State)
//! 6  RESOURCE_GRANT            component          ResourceKind         id
//! 7  RESOURCE_REVOKE           component          ResourceKind         id
//! 8  ENDPOINT_BIND             endpoint           provider             mechanism
//! 9  IRQ_ENTER                 irq                -                    -
//! 10 IRQ_DISPATCH              irq                component|ABSENT     -
//! 11 IRQ_ACK                   irq                -                    -
//! ```
//!
//! 未使用的词一律填 0（不是"缺省"），`flags` 必须为 0（留给未来扩展，
//! 不改变记录大小）。

use super::{RejectReason, TraceEvent, TraceRecord, TraceStats, capacity};
use crate::component::ComponentState;
use crate::component::endpoint::Mechanism;
use crate::resource::ResourceKind;

pub use crate::generated::abi::{
    ABSENT, KIND_COMPONENT_STATE, KIND_ENDPOINT_BIND, KIND_IRQ_ACK, KIND_IRQ_DISPATCH,
    KIND_IRQ_ENTER, KIND_POLICY_ACCEPTED, KIND_POLICY_PROPOSAL, KIND_POLICY_REJECTED,
    KIND_RESOURCE_GRANT, KIND_RESOURCE_REVOKE, KIND_TASK_SWITCH, TraceRecordAbi, TraceStatsAbi,
};

impl From<&TraceStats> for TraceStatsAbi {
    fn from(stats: &TraceStats) -> Self {
        Self {
            capacity: capacity() as u64,
            oldest_seq: stats.oldest_seq,
            next_seq: stats.next_seq,
            overwritten_total: stats.overwritten_total,
            enabled_mask: stats.enabled_mask,
        }
    }
}

const fn opt(value: Option<u64>) -> u64 {
    match value {
        Some(raw) => raw,
        None => ABSENT,
    }
}

const fn state_code(state: ComponentState) -> u64 {
    match state {
        ComponentState::Declared => 0,
        ComponentState::Resolved => 1,
        ComponentState::Starting => 2,
        ComponentState::Ready => 3,
        ComponentState::Stopping => 4,
        ComponentState::Stopped => 5,
        ComponentState::Failed => 6,
    }
}

const fn kind_code(kind: ResourceKind) -> u64 {
    match kind {
        ResourceKind::Device => 0,
        ResourceKind::Irq => 1,
        ResourceKind::Dma => 2,
    }
}

const fn reason_code(reason: RejectReason) -> u64 {
    match reason {
        RejectReason::NotRunnable => 0,
        RejectReason::OwnerNotRunnable => 1,
    }
}

/// Core 在 bind 时选定的调用机制（`endpoint::Mechanism`）的稳定编码。
const fn mechanism_code(mechanism: Mechanism) -> u64 {
    match mechanism {
        Mechanism::Direct => 0,
        Mechanism::Gate => 1,
    }
}

impl From<&TraceRecord> for TraceRecordAbi {
    fn from(record: &TraceRecord) -> Self {
        let (kind, a, b, c) = match record.event {
            TraceEvent::TaskSwitch { from, to } => (
                KIND_TASK_SWITCH,
                opt(from.map(|id| u64::from(id.raw()))),
                u64::from(to.raw()),
                0,
            ),
            TraceEvent::PolicyProposal { component, task } => (
                KIND_POLICY_PROPOSAL,
                u64::from(component.raw()),
                u64::from(task.raw()),
                0,
            ),
            TraceEvent::PolicyAccepted { component, task } => (
                KIND_POLICY_ACCEPTED,
                u64::from(component.raw()),
                u64::from(task.raw()),
                0,
            ),
            TraceEvent::PolicyRejected { component, reason } => (
                KIND_POLICY_REJECTED,
                u64::from(component.raw()),
                reason_code(reason),
                0,
            ),
            TraceEvent::ComponentState {
                component,
                from,
                to,
            } => (
                KIND_COMPONENT_STATE,
                u64::from(component.raw()),
                opt(from.map(state_code)),
                state_code(to),
            ),
            TraceEvent::ResourceGrant {
                component,
                kind,
                id,
            } => (
                KIND_RESOURCE_GRANT,
                u64::from(component.raw()),
                kind_code(kind),
                id,
            ),
            TraceEvent::ResourceRevoke {
                component,
                kind,
                id,
            } => (
                KIND_RESOURCE_REVOKE,
                u64::from(component.raw()),
                kind_code(kind),
                id,
            ),
            TraceEvent::EndpointBind {
                endpoint,
                provider,
                mechanism,
            } => (
                KIND_ENDPOINT_BIND,
                endpoint.raw(),
                u64::from(provider.raw()),
                mechanism_code(mechanism),
            ),
            TraceEvent::IrqEnter { irq } => (KIND_IRQ_ENTER, u64::from(irq), 0, 0),
            TraceEvent::IrqDispatch { irq, component } => (
                KIND_IRQ_DISPATCH,
                u64::from(irq),
                opt(component.map(|id| u64::from(id.raw()))),
                0,
            ),
            TraceEvent::IrqAck { irq } => (KIND_IRQ_ACK, u64::from(irq), 0, 0),
        };
        Self {
            seq: record.seq,
            timestamp: record.timestamp,
            kind,
            flags: 0,
            a,
            b,
            c,
        }
    }
}

#[cfg(test)]
mod tests;
