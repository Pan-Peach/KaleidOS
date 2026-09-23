//! Trace 事件类型与 payload。
//!
//! 事件只携带**类型化 ID 与理由**（`TaskId` / `ComponentId` / `EndpointId` /
//! `ResourceKind` / opaque resource id / `RejectReason`…），
//! 不携带格式化字符串或裸指针。Trace 面向机器、测试与性能分析，
//! 不是给人读的 log：绝不能靠反向解析字符串来还原系统行为。
//!
//! 第一阶段刻意只覆盖真正存在 chokepoint 的集合：
//! task switch / scheduler policy / component lifecycle / authority grant·revoke /
//! endpoint binding / IRQ。
//!
//! **未定义**：`TaskBlock` / `TaskWake` —— Core 目前没有 block/wake 路径
//! （`TaskState::Blocked` 不可达，状态机的合法边里也没有它）。等 block/wake
//! 真实落地、有了 chokepoint 再加，不预先定义无法产生的事件。
//! **未定义**：`Fault` —— 异常目前全部落在 arch 的 trap/panic 路径，Core 侧
//! 还没有 fault 提交点；等 Core 接管 fault 记录时再加。
//! 同理：不要为了"覆盖所有可能情况"把 enum 一次设计得过大。

use crate::component::ComponentId;
use crate::component::ComponentState;
use crate::component::endpoint::{EndpointId, Mechanism};
use crate::resource::ResourceKind;
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

    /// 授予资源归属（device claim / IRQ route / DMA mapping；id 由 Core 分配）。
    ResourceGrant {
        component: ComponentId,
        kind: ResourceKind,
        id: u64,
    },
    /// 回收资源归属（撤销后旧 id 一律失效）。
    ResourceRevoke {
        component: ComponentId,
        kind: ResourceKind,
        id: u64,
    },

    /// consumer 完成一次 endpoint 绑定解析（`EndpointRegistry::bind`）：Core 在
    /// 此刻按 `(caller 域, provider 域)` 选定调用机制（Direct / Gate）。
    ///
    /// 三个 payload 词都是 Core 真相：`endpoint` 是绑定身份（`EndpointId` 只在
    /// commit 后存在、永不重定向）；`provider` 是 endpoint owner（用于与组件
    /// 生命周期 / 资源事件关联）；`mechanism` 是 Core 在 bind 时做出的机制决定
    /// ——它是该决定的**唯一可观测点**（运行期不再重决策，`docs/architecture/
    /// deployment.md` §2）。consumer 身份不在本事件里：bind 需要 caller 边界，
    /// 但绑定真相只记 endpoint / provider / mechanism（与旧事件如实不记 consumer
    /// 边同理）。
    EndpointBind {
        endpoint: EndpointId,
        provider: ComponentId,
        mechanism: Mechanism,
    },

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
