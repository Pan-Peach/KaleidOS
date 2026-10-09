//! `Endpoint<FileSystem>` 的 typed 前端（consumer 侧）+ Core 选定的调用绑定。
//!
//! 业务代码只见 [`FileSystemBinding::mount`] / [`FileSystemBinding::open`] /
//! [`FileSystemBinding::read`] 等——method 号、frame、**调用机制**全部隐藏。
//! 调用机制（Direct / Gate）由 Core 在 [`Endpoint::bind`] 时选定一次，本层只实现，
//! 不选择：`Direct` 绑定直接调 provider function table（无 Core 介入、无分配、无
//! 打包），`Gate` 绑定走 `kcore_endpoint_call`——**调用点完全相同**。
//!
//! `read` 接受普通数据缓冲区。Direct 直接写入；Gate 在 SDK 内使用最多
//! 512 字节数据的临时 frame，校验长度后复制；允许短读和零长度缓冲区。
//!
//! # 错误分类（三类必须可区分）
//!
//! - [`InvokeError::Transport`]：Core 传输失败（Gate 绑定才有），provider **未被调用**；
//! - [`InvokeError::Method`]：provider 被调用并返回 `-errno`（或请求按契约无效，
//!   前端直接挡下：超长路径）；
//! - [`InvokeError::InvalidReply`]：传输成功但 provider / Core 的回复不是契约形状。

use core::ffi::CStr;

use crate::endpoint::{Endpoint, InvokeError};
use crate::errno::Errno;
use crate::filesystem::FileSystem;
use crate::filesystem::backend::{self, Backend};
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

    /// 从当前位置读到普通数据缓冲区，返回实际长度；Gate 单次最多读 512 字节。
    pub fn read(&self, handle: u64, buf: &mut [u8]) -> Result<usize, InvokeError> {
        backend::read(&self.backend, handle, buf)
    }

    /// 本次挂载的根节点 token；卸载后失效。
    pub fn root(&self) -> Result<u64, InvokeError> {
        backend::root(&self.backend)
    }

    /// 在 parent 中查找一个名字；编码与原生匹配规则由 provider 检查。
    pub fn lookup(&self, parent: u64, name: &[u8], encoding: u32) -> Result<u64, InvokeError> {
        if name.is_empty() || name.len() > crate::generated::filesystem::KCOMP_FILESYSTEM_NAME_MAX {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        backend::lookup(&self.backend, parent, name, encoding)
    }

    /// 返回 KCOMP_FILESYSTEM_NODE_* 类型。
    pub fn node_info(&self, node: u64) -> Result<u32, InvokeError> {
        backend::node_info(&self.backend, node)
    }
}

#[cfg(test)]
mod tests;
