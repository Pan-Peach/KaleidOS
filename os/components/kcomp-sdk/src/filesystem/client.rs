//! `Endpoint<FileSystem>` 的 typed 前端（consumer 侧）+ Core 选定的调用绑定。
//!
//! 业务代码只见 [`FileSystemBinding::mount`] / [`FileSystemBinding::open`] /
//! [`FileSystemBinding::read`] 等——method 号、frame、**调用机制**全部隐藏。
//! 调用机制（Direct / Gate）由 Core 在 [`Endpoint::bind`] 时选定一次，本层只实现，
//! 不选择：`Direct` 绑定直接调 provider function table（无 Core 介入、无分配、无
//! 打包），`Gate` 绑定走 `kcore_endpoint_call`——**调用点完全相同**。
//!
//! # `read` 的缓冲区布局
//!
//! `buf` **就是**扁平 frame 的 `output` 区：前
//! [`KCOMP_FILESYSTEM_READ_HEADER_LEN`](crate::generated::filesystem::KCOMP_FILESYSTEM_READ_HEADER_LEN)
//! （8）字节是 LE `u64` 实际长度头，数据从 offset 8 开始；成功返回实际长度，
//! 数据在 `&buf[8..8 + actual]`。`buf.len()` 含头，因此单次读的数据容量是
//! `buf.len() - 8`。Direct / Gate 两条机制对 consumer 呈现**同一份**缓冲区布局。
//!
//! # 错误分类（三类必须可区分）
//!
//! - [`InvokeError::Transport`]：Core 传输失败（Gate 绑定才有），provider **未被调用**；
//! - [`InvokeError::Method`]：provider 被调用并返回 `-errno`（或请求按契约无效，
//!   前端直接挡下：超长路径 / `read` 缓冲区放不下 8 字节头）；
//! - [`InvokeError::InvalidReply`]：传输成功但 provider / Core 的回复不是契约形状。

use core::ffi::CStr;

use crate::endpoint::{Endpoint, InvokeError};
use crate::errno::Errno;
use crate::filesystem::FileSystem;
use crate::filesystem::backend::{self, Backend};
use crate::filesystem::dispatch::is_read_output_len;
use crate::generated::filesystem::KCOMP_FILESYSTEM_PATH_MAX;

/// `filesystem` 的**调用绑定**（consumer 侧句柄）。
///
/// 内部持有 Core 在 bind 时选定的机制（Direct：provider function table + state；
/// Gate：opaque EndpointId）——机制是**私有**的：消费者拿不到裸 function table，
/// 也无法选择走哪条路。
pub struct FileSystemBinding {
    backend: Backend,
}

impl Endpoint<FileSystem> {
    /// bind：调 `kcore_endpoint_bind`——Core 做 **exact contract + abi + 存活**校验，
    /// 并按 `(caller domain, provider domain)` **一次性选定机制**（运行期不再重决策）。
    pub fn bind(&self) -> Result<FileSystemBinding, InvokeError> {
        Ok(FileSystemBinding {
            backend: backend::bind(self.id())?,
        })
    }
}

impl FileSystemBinding {
    /// 挂载该 filesystem 实例。
    pub fn mount(&self) -> Result<(), InvokeError> {
        backend::mount(&self.backend)
    }

    /// 卸载该 filesystem 实例。
    pub fn unmount(&self) -> Result<(), InvokeError> {
        backend::unmount(&self.backend)
    }

    /// 打开 `path`（NUL 结尾、相对该 filesystem root），返回不透明 file handle。
    ///
    /// `path` 的 NUL 结尾字节会一并进入 Gate 的 `input`（Direct 路径把它当 C
    /// 字符串）；超过
    /// [`KCOMP_FILESYSTEM_PATH_MAX`](crate::generated::filesystem::KCOMP_FILESYSTEM_PATH_MAX)
    /// 的路径在调用前就被挡下（不浪费一次传输，两条机制一致）。
    pub fn open(&self, path: &CStr, flags: u32) -> Result<u64, InvokeError> {
        if path.to_bytes_with_nul().len() > KCOMP_FILESYSTEM_PATH_MAX {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        backend::open(&self.backend, path, flags)
    }

    /// 关闭一个 [`FileSystemBinding::open`] 返回的 handle。
    pub fn close(&self, handle: u64) -> Result<(), InvokeError> {
        backend::close(&self.backend, handle)
    }

    /// 从 `handle` 当前位置读数据到 `buf` 的数据区（布局见模块文档）。
    ///
    /// 成功返回实际读到的字节数（数据在 `&buf[8..8 + actual]`）。`buf` 必须至少
    /// 放得下 8 字节长度头；不足时在调用前挡下（不浪费一次传输，两条机制一致）。
    pub fn read(&self, handle: u64, buf: &mut [u8]) -> Result<usize, InvokeError> {
        if !is_read_output_len(buf.len()) {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        backend::read(&self.backend, handle, buf)
    }
}

#[cfg(test)]
mod tests;
