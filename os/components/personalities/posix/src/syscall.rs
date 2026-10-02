//! syscall 解码 / POSIX 语义 / Linux errno 翻译由 personality 提供。
//! Core / Arch 只捕获用户 trap、验证真实调用来源并交付现场；路由 ABI 尚未定稿。
//! 不将未经校验的用户指针传给 VFS / provider 的 Direct table。

use crate::{Error, Result};

pub struct RawSyscall {
    pub number: usize,
    pub args: [usize; 6],
}

/// 必须来自 Core 的真实当前 task，再从本实例 thread 表定位进程；
/// 不能相信用户寄存器中的 pid / task id。当前尚未接入任何 trap。
pub struct SyscallContext {
    pub core_task: u32,
}

pub struct SyscallDispatcher;

impl SyscallDispatcher {
    /// TODO: 当前先返回 Unsupported；Linux syscall 号 / 布局 / 返回编码另行定稿。
    /// 不把 Rust enum / trait / 此结构当成 Core↔组件的 trap ABI。
    pub fn dispatch(&mut self, _context: &SyscallContext, _call: &RawSyscall) -> Result<isize> {
        Err(Error::Unsupported)
    }
}
