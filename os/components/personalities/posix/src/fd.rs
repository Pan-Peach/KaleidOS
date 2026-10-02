//! fd 是进程局部的整数；与 VFS 打开实例不同。每条 fd 记录持有一个用户引用。

use crate::process::ProcessId;
use crate::{Error, Result};

/// Console 的 transport / 授权尚未定稿；不能把 kcore_log_line 当作用户 stdout。
pub enum FdTarget {
    VfsFile(u64),
    Console,
}

pub struct FdEntry {
    pub process: ProcessId,
    pub number: i32,
    pub target: FdTarget,
    /// 进程局部的 FD_CLOEXEC；共享游标 / access/share 存在 VFS。
    pub close_on_exec: bool,
}

pub struct FdTable<'a> {
    pub entries: &'a mut [Option<FdEntry>],
}

impl FdTable<'_> {
    /// TODO: 在本进程分配 fd；失败时回滚 VFS open 引用。
    pub fn install(&mut self, _process: ProcessId, _target: FdTarget) -> Result<i32> {
        Err(Error::Unsupported)
    }

    /// TODO: VFS retain，然后安装新 fd；两者失败清理，保持同一 open / 游标。
    pub fn duplicate(&mut self, _process: ProcessId, _fd: i32) -> Result<i32> {
        Err(Error::Unsupported)
    }

    /// TODO: 先解除进程 fd，再消费一次 VFS 引用；区分用户引用与在途请求。
    pub fn close(&mut self, _process: ProcessId, _fd: i32) -> Result<()> {
        Err(Error::Unsupported)
    }
}
