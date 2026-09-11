//! 任务记录：Core 真相的载体。

use crate::component::ComponentId;
use crate::memory::MemoryLease;
use crate::task::Kernelstack;
use crate::task::state::TaskState;
use alloc::boxed::Box;
use arch::ContextImpl;

#[derive(Debug, PartialEq)]
pub struct TaskRecord {
    /// 创建该任务的组件。任务运行时的 caller identity 从这里解析，
    /// 不依赖 component loader 的 `call_init` 上下文。
    owner: ComponentId,
    /// Core-controlled truth：状态只能通过 Core 内部入口改变，组件（外部 crate）
    /// 无法直接赋值。调度器里程碑落地后在此之上加验证式 transition API。
    state: TaskState,
    pub context: Box<ContextImpl>,
    pub kstack: Kernelstack,
    pub(crate) memory: Option<MemoryLease>,
}

impl TaskRecord {
    pub(crate) fn new(
        owner: ComponentId,
        context: Box<ContextImpl>,
        kstack: Kernelstack,
        memory: MemoryLease,
    ) -> Self {
        Self {
            owner,
            state: TaskState::Created,
            context,
            kstack,
            memory: Some(memory),
        }
    }

    /// 只读观察任务归属。owner 是 Core 真相，不能由组件或调度策略修改。
    pub fn owner(&self) -> ComponentId {
        self.owner
    }

    /// 只读观察状态（monitor / trace / 调度器读侧）。
    pub fn state(&self) -> TaskState {
        self.state.clone()
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
