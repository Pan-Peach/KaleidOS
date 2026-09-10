//! Task 真相：身份（TaskId）、状态、运行 CPU、内核栈、上下文。
//! 调度策略数据（runqueue、vruntime 等）不在此模块 —— 属于 Scheduler Component。
//! 调度 commit 路径（propose → validate → commit → switch）在 `crate::sched`。

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

use crate::component::{ComponentId, ComponentState};

pub static TASK_TABLE: spin::Once<spin::Mutex<TaskTable>> = spin::Once::new();

pub fn init() {
    TASK_TABLE.call_once(|| spin::Mutex::new(TaskTable::new()));
}

pub fn get_task_table() -> &'static spin::Mutex<TaskTable> {
    TASK_TABLE.get().expect("task table not initialized")
}

/// Core 语义入口：创建任务（组件只能经 export ABI `kcore_task_create` 到达）。
///
/// 验证（Core validates，组件只有提议权）：
/// 1. `requester` 必须存在且处于 `Ready` —— 只有活着的组件能创建任务；
/// 2. `entry` 必须落在该组件的**装载镜像内**（`[base, base+size)`）——
///    组件不能把执行权指到任意内核地址，也不能指到别的组件的镜像。
///
/// 通过后由 `TaskTable::create` 分配 id + 内核栈 + 初始上下文（`Created` 态，
/// 经 `transition(Created→Runnable)` 后进入调度）。
///
/// # Seam
/// 现在"requester 是谁"来自 `component::load::current_component()`（call_init
/// 期间 Core 记录的身份）。真正的 per-execution-domain 凭证（TaskHandle 化）
/// 留给未来 ExecutionDomain 里程碑。
pub fn create_task(requester: ComponentId, entry: usize) -> Result<TaskId, TaskError> {
    let registry = crate::component::registry::get_registry().lock();
    let record = registry
        .get(requester)
        .ok_or(TaskError::RequesterNotFound)?;
    if record.state != ComponentState::Ready {
        return Err(TaskError::RequesterNotReady);
    }
    let Some(lease) = &record.memory else {
        // 无装载镜像（不应发生：Ready 组件必然已经 loader 放段）。
        return Err(TaskError::EntryOutOfImage);
    };
    let image = lease.region();
    if entry < image.base || entry >= image.base + image.size {
        return Err(TaskError::EntryOutOfImage);
    }
    drop(registry);

    get_task_table().lock().create(entry)
}
