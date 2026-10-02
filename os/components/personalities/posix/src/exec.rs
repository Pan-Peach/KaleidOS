//! 用户可执行文件装载器接缝，属于 personality，不复用 Core 的 ET_REL .kcomp loader。
//! 第一目标为 RV64 静态 ELF / 单进程；libc 仍需要 Linux syscall 与启动 ABI。

use crate::usermem::UserAddress;
use crate::{Error, Result};
use kcomp_sdk::vfs::VfsPath;

pub struct ExecRequest<'a> {
    pub executable: VfsPath,
    pub argv: &'a [&'a [u8]],
    pub envp: &'a [&'a [u8]],
}

pub struct LoadSegment {
    pub file_offset: u64,
    pub file_size: u64,
    pub memory_size: u64,
    pub user_address: UserAddress,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
}

/// 只表示校验后的装载计划，不授予页表修改权，也不代表已创建用户任务。
pub struct LoadPlan {
    pub entry: UserAddress,
    pub segment_count: usize,
}

pub struct ImageLoader;

impl ImageLoader {
    /// TODO: ELF class/machine/type、段边界/溢出/重叠、BSS、W^X；拒绝 PT_INTERP。
    /// 先支持静态 ET_EXEC；动态链接 / static PIE 未在第一阶段范围内。
    pub fn inspect(&self, _image: &[u8], _segments: &mut [LoadSegment]) -> Result<LoadPlan> {
        Err(Error::Unsupported)
    }

    /// TODO: 经 VFS 读取、提交 Core mapping 提议、复制段、构造 argc/argv/envp/auxv。
    pub fn prepare(&self, _request: &ExecRequest<'_>) -> Result<LoadPlan> {
        Err(Error::Unsupported)
    }
}
