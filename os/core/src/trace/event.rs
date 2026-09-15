//! Trace 事件类型与 payload。
//!
//! 事件只携带**类型化 ID 与理由**（`TaskId` / `ComponentId` / `InterfaceId` /
//! `BindingId` / `ResourceKind` / `RawHandle` / `RejectReason`…），
//! 不携带格式化字符串或裸指针。Trace 面向机器、测试与性能分析，
//! 不是给人读的 log：绝不能靠反向解析字符串来还原系统行为。
//!
//! 第一阶段刻意只覆盖真正存在 chokepoint 的集合：
//! task switch / scheduler policy / component lifecycle / authority grant·revoke /
//! interface binding / IRQ。
//!
//! **未定义**：`TaskBlock` / `TaskWake` —— Core 目前没有 block/wake 路径
//! （`TaskState::Blocked` 不可达，状态机的合法边里也没有它）。等 block/wake
//! 真实落地、有了 chokepoint 再加，不预先定义无法产生的事件。
//! **未定义**：`Fault` —— 异常目前全部落在 arch 的 trap/panic 路径，Core 侧
//! 还没有 fault 提交点；等 Core 接管 fault 记录时再加。
//! 同理：不要为了"覆盖所有可能情况"把 enum 一次设计得过大。

use crate::component::ComponentId;
use crate::component::ComponentState;
use crate::component::interface::{BindingId, InterfaceId};
use crate::handle::{RawHandle, ResourceKind};
use crate::task::TaskId;

/// Core 拒绝一次提议的理由（Core owns truth：理由由 Core 判定，不由组件自报）。
///
/// 与 `errno` 的映射是另一层（ABI 返回值）；这里只记录语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// 提议的任务不在 Core 当前的 runnable 集合里（存在性验证失败）。
    NotRunnable,
    /// 提交前复检发现该任务的 owner 已不再是活组件（活实例门禁失败）。
    OwnerNotRunnable,
}

/// 一个结构化事件。全部字段是值（无指针、无字符串），因此 `Copy` 且可比较。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    /// 当前任务发生切换（`from` 为 `None` 表示从"无当前任务"进入）。
    TaskSwitch { from: Option<TaskId>, to: TaskId },

    /// 调度策略组件提议了下一个任务（**提议**，尚未生效）。
    PolicyProposal {
        component: ComponentId,
        task: TaskId,
    },
    /// Core 验证通过并采纳该提议。
    PolicyAccepted {
        component: ComponentId,
        task: TaskId,
    },
    /// Core 拒绝该提议（真相不变；`component` 是被拒绝的提议方）。
    PolicyRejected {
        component: ComponentId,
        reason: RejectReason,
    },

    /// 组件生命周期状态提交（`Registry::transition` 的唯一切点）。
    /// `from` 为 `None` 表示这是组件**出生**（`declare` 登记为 `Declared`）。
    ComponentState {
        component: ComponentId,
        from: Option<ComponentState>,
        to: ComponentState,
    },

    /// 授予 authority（handle 由 Core 分配）。
    ResourceGrant {
        component: ComponentId,
        kind: ResourceKind,
        handle: RawHandle,
    },
    /// 回收 authority（撤销后旧 handle 一律 Stale）。
    ResourceRevoke {
        component: ComponentId,
        kind: ResourceKind,
        handle: RawHandle,
    },

    /// consumer 完成一次 interface 绑定解析（`InterfaceRegistry::bind`）。
    ///
    /// `consumer` 为 `None` 是**如实**的：Core 不记录 consumer→provider 边
    /// （`bind` 是无状态查询，只证明"此刻能拿到 provider"）。consumer 身份只
    /// 存在于 ABI 边界（`RequestContext::ambient`），不在这份绑定真相里。
    InterfaceBind {
        consumer: Option<ComponentId>,
        provider: ComponentId,
        interface: InterfaceId,
    },
    /// 已绑定 interface 的 provider/generation 刷新（热替换后重新解析）。
    InterfaceRefresh { binding: BindingId, generation: u64 },

    /// 外部中断进入 Core（trap 交接边界）。
    IrqEnter { irq: u32 },
    /// Core 把该 IRQ 路由给某个组件（回调投递或轮询唤醒）。
    IrqDispatch {
        irq: u32,
        component: Option<ComponentId>,
    },
    /// 该 IRQ 线完成 ack（控制器 complete）。
    IrqAck { irq: u32 },
}
