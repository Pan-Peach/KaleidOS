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
    /// start 时 Core 提交的固定 CPU。首次 dispatch 和 wake 后重入均需匹配。
    /// `None` 只用于 Created / Core 内部尚未放置的 fresh context；第一次 dispatch
    /// 也会固定归属，防止 outgoing 保存完成前被远端 CPU 取走。
    home_cpu: Option<CpuId>,
    /// `unpark` 早于 `park` 时暂存的一次通知；重复通知合并为一个 permit。
    park_pending: bool,
    /// 组件任务入口（`KcompTaskEntry`：`void (*)(void *)`），由
    /// `kcore_task_create` 提供并验证落在 owner 的装载镜像内。
    entry: usize,
    /// opaque 参数：Core 原样透传给入口；**任务归属与它无关**（来自 Core 的
    /// 执行边界 = 本记录的 owner）。
    arg: *mut (),
    pub context: Box<ContextImpl>,
    /// Interrupt state of this suspended execution; fresh tasks enable IRQs.
    pub(crate) irq_flags: Option<<arch::CpuImpl as arch::CpuArch>::IrqFlags>,
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
            home_cpu: None,
            park_pending: false,
            entry,
            arg,
            context,
            irq_flags: None,
            kstack,
            memory: Some(memory),
        }
    }

    /// 只读观察任务归属。owner 是 Core 真相，不能由组件或调度策略修改。
    pub fn owner(&self) -> ComponentId {
        self.owner
    }

    /// 任务固定的逻辑 CPU（`None` = 尚未设置归属）。
    pub fn home_cpu(&self) -> Option<CpuId> {
        self.home_cpu
    }

    /// 该任务此刻能否被逻辑 CPU `cpu` 认领（`Runnable → Running` 的前置条件）。
    ///
    /// - 尚未设置归属（`home_cpu == None`）：Core 内部 fresh context 可首次认领；
    /// - 已固定归属：只有目标 CPU 可认领（跨 CPU 认领会踩离场上下文竞态）。
    pub fn claimable_by(&self, cpu: CpuId) -> bool {
        self.home_cpu.is_none_or(|last| last == cpu)
    }

    /// Core 内部写入点：任务被提交为 `Running(cpu)` 时记录/更新归属 CPU。
    pub(crate) fn set_home_cpu(&mut self, cpu: CpuId) {
        self.home_cpu = Some(cpu);
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
