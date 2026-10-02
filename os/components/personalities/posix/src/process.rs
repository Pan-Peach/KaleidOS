//! 进程语义归 POSIX；task 的存在 / 状态 / AS 切换归 Core。

use kcomp_sdk::vfs::VfsPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadId(pub u32);

pub enum ProcessState {
    Preparing,
    Running,
    Exited { code: i32 },
}

pub struct Process {
    pub id: ProcessId,
    pub parent: Option<ProcessId>,
    pub state: ProcessState,
    pub root: VfsPath,
    pub cwd: VfsPath,
}

pub struct Thread {
    pub id: ThreadId,
    pub process: ProcessId,
    /// Core 返回的 TaskId；当前没有用户任务 / AS 绑定机制，不能自行填写生效。
    pub core_task: Option<u32>,
}

/// Core 提供已验证的机制后，由 personality 提议创建 / 装载，不直接改页表。
pub struct UserExecution;

impl UserExecution {
    /// TODO: 用户 AS、task 绑定、初始 trap frame、执行域与 syscall 路由。
    pub fn start(
        &mut self,
        _process: ProcessId,
        _image: &crate::exec::LoadPlan,
    ) -> crate::Result<ThreadId> {
        Err(crate::Error::Unsupported)
    }

    /// TODO: 退出语义、fd 引用释放、停止任务；不把退出等同于物理 backing 回收。
    pub fn exit(&mut self, _thread: ThreadId, _code: i32) -> crate::Result<()> {
        Err(crate::Error::Unsupported)
    }
}
