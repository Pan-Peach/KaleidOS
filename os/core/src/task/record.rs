//! 任务记录：Core 真相的载体。

use crate::memory::MemoryLease;
use crate::task::Kernelstack;
use crate::task::state::TaskState;
use alloc::boxed::Box;
use arch::ContextImpl;

#[derive(Debug, PartialEq)]
pub struct TaskRecord {
    /// Core-controlled truth：状态只能通过 Core 内部入口改变，组件（外部 crate）
    /// 无法直接赋值。调度器里程碑落地后在此之上加验证式 transition API。
    state: TaskState,
    pub context: Box<ContextImpl>,
    pub kstack: Kernelstack,
    pub(crate) memory: Option<MemoryLease>,
}

impl TaskRecord {
    pub(crate) fn new(context: Box<ContextImpl>, kstack: Kernelstack, memory: MemoryLease) -> Self {
        Self {
            state: TaskState::Created,
            context,
            kstack,
            memory: Some(memory),
        }
    }

    /// 只读观察状态（monitor / trace / 调度器读侧）。
    pub fn state(&self) -> TaskState {
        self.state.clone()
    }

    /// Core 内部写入点。当前只保证"组件改不了"；
    /// 合法/非法转换的验证（Running(cpu) 互斥、Exited 终态等）随调度器
    /// 的 transition API 一起由人类实现（docs/testing.md §6）。
    /// 现在唯一的调用方是 table 测试与未来的调度器 commit 路径。
    #[allow(dead_code)]
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
