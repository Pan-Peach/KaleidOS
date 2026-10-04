//! Minimal RV64 process profile: static ELF, fork/exec/wait and console syscalls.
//! 实现入口见 docs/modules/posix.md；用户态路线见 docs/development/userspace.md。
//! Core owns task / AS / trap truth; the general VFS/fd models remain placeholders.

#![no_std]
extern crate alloc;

pub mod exec;
pub mod execution;
pub mod fd;
pub mod image;
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
    BadExecutable,
    BadDescriptor,
    BadUserMemory,
    NoSuchProcess,
    Vfs(kcomp_sdk::vfs::VfsError),
}

pub type Result<T> = core::result::Result<T, Error>;

/// 表存储由本实例拥有；不使用静态全局 PCB / fd 表。
/// Future VFS profile model. The current immutable-image profile uses execution::Family.
pub struct PosixState<'a> {
    pub vfs: Option<VfsBinding>,
    pub processes: &'a mut [Option<Process>],
    pub threads: &'a mut [Option<Thread>],
    pub descriptors: &'a mut [Option<fd::FdEntry>],
}
