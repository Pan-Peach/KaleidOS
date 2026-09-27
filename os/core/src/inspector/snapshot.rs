//! Inspector 的观察结果 —— 全部是**值拷贝快照**，不含对 Core 内部结构的引用。
//!
//! 快照是某一时刻的一致读：调用方只能据此断言，不能回写。类型上就杜绝了回写
//! ——所有快照都**没有生命周期参数**，拿不到任何 Core 内部结构的引用。
//!
//! 快照是**读模型**：允许把同一条真相投影成更好断言的形状（例如把
//! `TaskState::Running(cpu)` 投影成 `running_on`，或把实例引用的 image 字段
//! 投影进实例快照），但绝不引入第二条真相。

use crate::component::{ComponentId, ComponentState};
use crate::machine::CpuId;
use crate::task::{TaskId, TaskState};

/// 一个任务的只读快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub id: TaskId,
    /// 创建该任务的组件**实例**（Core 真相；任务运行时的 caller identity 来源）。
    pub owner: ComponentId,
    /// 任务状态 —— 唯一真相。
    pub state: TaskState,
    /// `state` 的**投影**：`Running(cpu)` 时为 `Some(cpu)`，否则 `None`。
    /// 不是独立字段，只是让"在哪个 CPU 上跑"读起来更直白。
    pub running_on: Option<CpuId>,
}

/// 一个组件（`ComponentId`）的只读快照：生命周期真相 + 它自己 loaded image 的
/// 投影。loaded image 1:1 归属该组件，不存在第二层 image 身份。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentSnapshot {
    pub id: ComponentId,
    pub state: ComponentState,
    /// `kcomp_instance_create` 写回的 opaque state 指针，**只作为数值观察**
    /// （快照是值拷贝；Core 与观察者都绝不解引用）。
    pub instance_state: usize,
    /// 以下为 loaded image 投影：判断"这段代码/入口在哪"。
    pub base: usize,
    pub create: usize,
    pub destroy: usize,
    pub text_size: usize,
    pub abi: u64,
}

/// 一个物理内存 region 的只读快照（来自已提交的 `MachineInfo`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegionSnapshot {
    pub base: usize,
    pub size: usize,
}
