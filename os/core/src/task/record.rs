//! 任务记录：Core 真相的载体。

use crate::memory::MemoryLease;
use crate::task::Kernelstack;
use crate::task::state::TaskState;
use alloc::boxed::Box;
use arch::ContextImpl;

#[derive(Debug, PartialEq)]
pub struct TaskRecord {
    pub state: TaskState,
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
}

impl Drop for TaskRecord {
    fn drop(&mut self) {
        if let Some(lease) = self.memory.take() {
            let _ = crate::memory::free_region(lease);
        }
    }
}
