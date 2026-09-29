//! 任务记录：Core 真相的载体。

use crate::component::ComponentId;
use crate::machine::CpuId;
use crate::memory::MemoryLease;
use crate::task::Kernelstack;
use crate::task::state::TaskState;
use alloc::boxed::Box;
use arch::ContextImpl;

#[derive(Debug, PartialEq)]
pub struct TaskRecord {
    /// 创建该任务的组件**实例**。任务运行时的 caller identity 从这里解析，
    /// 不依赖 create 调用上下文。多个实例共享一个 image 时，owner 仍是实例。
    owner: ComponentId,
    /// Core-controlled truth：状态只能由 `TaskTable::transition` 验证后改变，
    /// 组件（外部 crate）无法直接赋值。
    state: TaskState,
    /// 任务上次运行所在的逻辑 CPU。
    ///
    /// `None` = 尚未运行过：它的上下文是创建时的 fresh 上下文，**任何 CPU 都可
    /// 认领**；`Some(cpu)` = 已在该 CPU 上跑过，**只能由该 CPU 再认领**。这条规则
    /// 规避「离场任务的上下文尚未保存完成就被另一 CPU 取走」的竞态（plan / Oracle
    /// #1）——首次运行前的上下文不需要保存，所以可以安全跨 CPU。
    last_cpu: Option<CpuId>,
    /// `unpark` 早于 `park` 时暂存的一次通知；重复通知合并为一个 permit。
    park_pending: bool,
    /// 组件任务入口（`KcompTaskEntry`：`void (*)(void *)`），由
    /// `kcore_task_create` 提供并验证落在 owner 的装载镜像内。
    entry: usize,
    /// opaque 参数：Core 原样透传给入口；**任务归属与它无关**（来自 Core 的
    /// 执行边界 = 本记录的 owner）。
    arg: *mut (),
    pub context: Box<ContextImpl>,
    pub kstack: Kernelstack,
    pub(crate) memory: Option<MemoryLease>,
}

// `arg` 是组件 opaque 指针：Core 只存/透传、永不解引用。跨线程使用由
// `TASK_TABLE` 的 Mutex 串行化（与 endpoint.rs 的 EndpointRecord 同一理由）。
unsafe impl Send for TaskRecord {}
unsafe impl Sync for TaskRecord {}

impl TaskRecord {
    pub(crate) fn new(
        owner: ComponentId,
        entry: usize,
        arg: *mut (),
        context: Box<ContextImpl>,
        kstack: Kernelstack,
        memory: MemoryLease,
    ) -> Self {
        Self {
            owner,
            state: TaskState::Created,
            last_cpu: None,
            park_pending: false,
            entry,
            arg,
            context,
            kstack,
            memory: Some(memory),
        }
    }

    /// 只读观察任务归属。owner 是 Core 真相，不能由组件或调度策略修改。
    pub fn owner(&self) -> ComponentId {
        self.owner
    }

    /// 任务上次运行的逻辑 CPU（`None` = 从未运行过）。
    pub fn last_cpu(&self) -> Option<CpuId> {
        self.last_cpu
    }

    /// 该任务此刻能否被逻辑 CPU `cpu` 认领（`Runnable → Running` 的前置条件）。
    ///
    /// - 从未运行（`last_cpu == None`）：任何 CPU 可认领（上下文 fresh）；
    /// - 运行过：只有其上次所在 CPU 可再认领（跨 CPU 认领会踩离场上下文竞态）。
    pub fn claimable_by(&self, cpu: CpuId) -> bool {
        self.last_cpu.is_none_or(|last| last == cpu)
    }

    /// Core 内部写入点：任务被提交为 `Running(cpu)` 时记录/更新归属 CPU。
    pub(crate) fn set_last_cpu(&mut self, cpu: CpuId) {
        self.last_cpu = Some(cpu);
    }

    /// 只读观察状态（monitor / trace / 调度器读侧）。
    pub fn state(&self) -> TaskState {
        self.state.clone()
    }

    pub(crate) fn park_pending(&self) -> bool {
        self.park_pending
    }

    /// Core 内部写入点：permit 与任务状态由同一张 TaskTable 管理。
    pub(crate) fn set_park_pending(&mut self, pending: bool) {
        self.park_pending = pending;
    }

    /// 组件任务入口地址（Core trampoline 读取后调用）。
    pub(crate) fn entry(&self) -> usize {
        self.entry
    }

    /// opaque 任务参数（Core trampoline 原样透传）。
    pub(crate) fn arg(&self) -> *mut () {
        self.arg
    }

    /// Core 内部写入点：组件（外部 crate）拿不到 `&mut`，改不了状态。
    /// 合法转换由 `TaskTable::transition` 验证，这里是唯一落笔处。
    pub(crate) fn set_state(&mut self, state: TaskState) {
        self.state = state;
    }
}

impl Drop for TaskRecord {
    fn drop(&mut self) {
        if let Some(lease) = self.memory.take() {
            let _ = crate::memory::free_region(lease);
        }
    }
}
