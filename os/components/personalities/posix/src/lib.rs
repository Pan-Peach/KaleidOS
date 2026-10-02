//! POSIX personality 骨架；当前不提供进程或 Linux syscall 兼容。
//! 实现入口见 docs/modules/posix.md；用户态路线见 docs/development/userspace.md。
//! 只声明组件内语义状态。Core 仍拥有 task / AS / trap / 生命周期真相。

#![no_std]

pub mod exec;
pub mod fd;
pub mod process;
pub mod syscall;
pub mod usermem;

#[cfg(not(test))]
mod runtime;

use kcomp_sdk::vfs::VfsBinding;
use process::{Process, Thread};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    BadDescriptor,
    BadUserMemory,
    NoSuchProcess,
    Vfs(kcomp_sdk::vfs::VfsError),
}

pub type Result<T> = core::result::Result<T, Error>;

/// 表存储由本实例拥有；不使用静态全局 PCB / fd 表。
/// TODO: 解析 SDK PosixCreateConfig，校验并 bind 指定 VFS；不扫描 / 自动创建 VFS。
pub struct PosixState<'a> {
    pub vfs: Option<VfsBinding>,
    pub processes: &'a mut [Option<Process>],
    pub threads: &'a mut [Option<Thread>],
    pub descriptors: &'a mut [Option<fd::FdEntry>],
}
