//! Task 真相：身份（TaskId）、状态、运行 CPU、内核栈、上下文。
//! 调度策略数据（runqueue、vruntime 等）不在此模块 —— 属于 Scheduler Component。
//! 状态机、跨 CPU 检查等真相逻辑由人类实现。

pub mod error;
pub mod id;
pub mod kstack;
pub mod record;
pub mod state;
pub mod table;

pub use error::TaskError;
pub use id::TaskId;
pub use kstack::Kernelstack;
pub use record::TaskRecord;
pub use state::TaskState;
pub use table::TaskTable;

pub static TASK_TABLE: spin::Once<spin::Mutex<TaskTable>> = spin::Once::new();

pub fn init() {
    TASK_TABLE.call_once(|| spin::Mutex::new(TaskTable::new()));
}

pub fn get_task_table() -> &'static spin::Mutex<TaskTable> {
    TASK_TABLE.get().expect("task table not initialized")
}
