//! 任务记录：Core 真相的载体。

use crate::task::Kernelstack;
use crate::task::state::TaskState;
use alloc::boxed::Box;
use arch::ContextImpl;

#[derive(Debug, PartialEq)]
pub struct TaskRecord {
    pub state: TaskState,
    pub context: Box<ContextImpl>,
    pub kstack: Kernelstack,
}

impl TaskRecord {
    pub fn new(context: Box<ContextImpl>, kstack: Kernelstack) -> Self {
        Self {
            state: TaskState::Created,
            context,
            kstack,
        }
    }
}
