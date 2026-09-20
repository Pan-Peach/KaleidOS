//! 组件侧的**稳定 trace 记录编码**（组件 ABI 的地基）。
//!
//! 约束（AGENTS.md）：跨 ABI 边界**禁止** Rust enum layout、mangled symbol、
//! trait object、指针。所以这里是一个 `#[repr(C)]` 的**固定形状**记录：
//! 稳定的 `kind` 标签 + 三个 64 位 payload 词；缺省值用 [`ABSENT`] 显式表达，
//! 而不是依赖 `Option` 的 niche / 布局（那是 Rust 的实现细节，不是契约）。
//!
//! `kind` 取值与 payload 分配是 **ABI 契约的一部分**：只增不改。本阶段不维护
//! 兼容，契约变了就原地替换（不保留旧编号）。
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
//! 8  INTERFACE_BIND            consumer|ABSENT    provider             interface
//! 9  INTERFACE_REFRESH         binding            generation           -
//! 10 IRQ_ENTER                 irq                -                    -
//! 11 IRQ_DISPATCH              irq                component|ABSENT     -
//! 12 IRQ_ACK                   irq                -                    -
//! ```
//!
//! 未使用的词一律填 0（不是"缺省"），`flags` 必须为 0（留给未来扩展，
//! 不改变记录大小）。

use super::{RejectReason, TraceEvent, TraceRecord, TraceStats, capacity};
use crate::component::ComponentState;
use crate::resource::ResourceKind;

/// payload 词里的"该字段不存在"哨兵。
pub const ABSENT: u64 = u64::MAX;

/// `TaskSwitch`：当前任务发生切换。
pub const KIND_TASK_SWITCH: u32 = 1;
/// `PolicyProposal`：调度策略提议。
pub const KIND_POLICY_PROPOSAL: u32 = 2;
/// `PolicyAccepted`：Core 采纳提议。
pub const KIND_POLICY_ACCEPTED: u32 = 3;
/// `PolicyRejected`：Core 拒绝提议。
pub const KIND_POLICY_REJECTED: u32 = 4;
/// `ComponentState`：组件生命周期状态提交。
pub const KIND_COMPONENT_STATE: u32 = 5;
/// `ResourceGrant`：授予 authority。
pub const KIND_RESOURCE_GRANT: u32 = 6;
/// `ResourceRevoke`：回收 authority。
pub const KIND_RESOURCE_REVOKE: u32 = 7;
/// `InterfaceBind`：一次成功的 interface 绑定解析。
pub const KIND_INTERFACE_BIND: u32 = 8;
/// `InterfaceRefresh`：provider/generation 刷新。
pub const KIND_INTERFACE_REFRESH: u32 = 9;
/// `IrqEnter`：外部中断进入 Core。
pub const KIND_IRQ_ENTER: u32 = 10;
/// `IrqDispatch`：Core 把 IRQ 路由给某组件。
pub const KIND_IRQ_DISPATCH: u32 = 11;
/// `IrqAck`：IRQ 线完成 ack。
pub const KIND_IRQ_ACK: u32 = 12;

/// 一条 trace 记录的稳定 ABI 形态。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceRecordAbi {
    /// Core 分配的单调序号（**排序与断言的唯一依据**）。
    pub seq: u64,
    /// 平台时钟原始值（单位/来源见 `BENCH-ENV` 风格的环境描述）。
    pub timestamp: u64,
    /// 事件标签（见上方分配表）。
    pub kind: u32,
    /// 保留；必须为 0。
    pub flags: u32,
    pub a: u64,
    pub b: u64,
    pub c: u64,
}

/// Trace 子系统状态的稳定 ABI 形态（`kcore_trace_stats` 的 out 结构）。
///
/// 字段全部显式编码。`overwritten_total` 是**因 ring 满被逐出保留区**的记录
/// 总数（saturating），不是"某个 reader 漏掉的条数"——reader 的真实缺口是
/// `returned_seq - requested_seq`。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceStatsAbi {
    /// ring 容量（records）——见 [`capacity`]。
    pub capacity: u64,
    /// 最旧存活记录的 `seq`；ring 为空时 == `next_seq`。
    pub oldest_seq: u64,
    /// 下一条记录将拿到的 `seq`。
    pub next_seq: u64,
    /// 因 ring 满被逐出保留区的记录总数。
    pub overwritten_total: u64,
    /// 已使能事件掩码：bit i ↔ 事件 kind i+1（上方 `KIND_*` 分配表），高位保留
    /// 恒 0；默认全开（`ENABLED_MASK_ALL`），`CONFIG_TRACE=n` 时恒 0。
    /// 过滤只决定采集与否：不记录、不消耗 `seq`，不算丢失。
    pub enabled_mask: u64,
}

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
            TraceEvent::InterfaceBind {
                consumer,
                provider,
                interface,
            } => (
                KIND_INTERFACE_BIND,
                opt(consumer.map(|id| u64::from(id.raw()))),
                u64::from(provider.raw()),
                u64::from(interface.raw()),
            ),
            TraceEvent::InterfaceRefresh {
                binding,
                generation,
            } => (
                KIND_INTERFACE_REFRESH,
                u64::from(binding.raw()),
                generation,
                0,
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
