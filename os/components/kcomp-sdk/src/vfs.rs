//! VFS 只读契约声明。ABI 单一来源为 `abi/vfs.toml`；编码见
//! `docs/interfaces/vfs.md`。内部 Rust 对象不会跨组件边界。
//!
//! 只通过真实 Request/Reply IPC 消费服务；Path 与 Open 引用绑定调用 Task。
//! 不支持的目录枚举、写入和命名流返回 ENOTSUP。

use crate::abi::InterfaceKind;
use crate::endpoint::InvokeError;
use crate::endpoint::{Contract, Endpoint};
use crate::errno::Errno;
use crate::generated::vfs_wire as wire;

pub use crate::generated::vfs::KCOMP_VFS_NAME as VFS_NAME;
pub use crate::generated::vfs::*;

pub struct Vfs;

impl Contract for Vfs {
    const ID: u64 = KCOMP_VFS_CONTRACT;
    const ABI: u64 = KCOMP_VFS_ABI;
    const KIND: InterfaceKind = InterfaceKind::Service;
}

/// 保留传输 / 原生 errno / VFS domain-status 的区别，不经过 Errno::from_code
/// 将未知 provider errno 归一化。具体 POSIX / NT 错误翻译归 personality。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    Transport(Errno),
    /// 负的原生 -errno。
    Method(i32),
    /// 方法仍返回负 errno；回复头补充 schema 列出的非零 domain 分类。
    Domain {
        errno: i32,
        detail: u32,
    },
    /// read_dir 的 -ENOBUFS，同时保留所需名字字节数；不返回截断名字。
    BufferTooSmall {
        required: usize,
    },
    InvalidReply,
}

pub type VfsResult<T> = core::result::Result<T, VfsError>;

pub mod codec;
/// Fixed IPC endpoint; never reconnect an old object to a new instance.
pub struct VfsBinding {
    pub endpoint: Endpoint<Vfs>,
}
impl VfsBinding {
    pub fn bind(endpoint: Endpoint<Vfs>) -> crate::Result<Self> {
        Ok(Self { endpoint })
    }
    pub fn root(&self) -> VfsResult<VfsPath> {
        let (status, reply) = wire::root(self.endpoint.id()).map_err(invoke_error)?;
        complete(status, reply.reply_status)?;
        Ok(reply.token)
    }
    /// Composition control only; drains opens and ends the owned server Task.
    pub fn shutdown(&self) -> VfsResult<()> {
        let (status, reply) = wire::shutdown(self.endpoint.id()).map_err(invoke_error)?;
        complete(status, reply)
    }
    pub fn resolve(&self, request: &VfsLookup, path: &[u8]) -> VfsResult<VfsPath> {
        let (status, reply) =
            wire::resolve(self.endpoint.id(), *request, path).map_err(invoke_error)?;
        complete(status, reply.reply_status)?;
        Ok(reply.token)
    }
    pub fn retain_path(&self, path: &VfsPath) -> VfsResult<()> {
        let (status, reply) = wire::retain_path(self.endpoint.id(), *path).map_err(invoke_error)?;
        complete(status, reply)
    }
    pub fn release_path(&self, path: &VfsPath) -> VfsResult<()> {
        let (status, reply) =
            wire::release_path(self.endpoint.id(), *path).map_err(invoke_error)?;
        complete(status, reply)
    }
    pub fn node_info(&self, path: &VfsPath) -> VfsResult<VfsNodeInfo> {
        let (status, reply) = wire::node_info(self.endpoint.id(), *path).map_err(invoke_error)?;
        complete(status, reply.reply_status)?;
        Ok(reply.info)
    }
    pub fn open(&self, request: &VfsOpenRequest, name: &[u8]) -> VfsResult<u64> {
        let (status, reply) =
            wire::open(self.endpoint.id(), *request, name).map_err(invoke_error)?;
        complete(status, reply.reply_status)?;
        if reply.file == 0 {
            Err(VfsError::InvalidReply)
        } else {
            Ok(reply.file)
        }
    }
    pub fn retain(&self, file: u64) -> VfsResult<()> {
        let (status, reply) = wire::retain(self.endpoint.id(), file).map_err(invoke_error)?;
        complete(status, reply)
    }
    pub fn close(&self, file: u64) -> VfsResult<()> {
        let (status, reply) = wire::close(self.endpoint.id(), file).map_err(invoke_error)?;
        complete(status, reply)
    }
    pub fn set_position(&self, file: u64, offset: u64) -> VfsResult<()> {
        let (status, reply) =
            wire::set_position(self.endpoint.id(), file, offset).map_err(invoke_error)?;
        complete(status, reply)
    }
    pub fn read(&self, file: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        let count = buffer.len().min(512);
        let mut output = [0; 512];
        let (status, reply) =
            wire::read(self.endpoint.id(), file, &mut output[..count]).map_err(invoke_error)?;
        complete(status, reply.reply_status)?;
        copy_read(buffer, &output[..count], reply.actual)
    }
    pub fn read_at(&self, file: u64, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        let count = buffer.len().min(512);
        let mut output = [0; 512];
        let (status, reply) = wire::read_at(self.endpoint.id(), file, offset, &mut output[..count])
            .map_err(invoke_error)?;
        complete(status, reply.reply_status)?;
        copy_read(buffer, &output[..count], reply.actual)
    }
    pub fn stream_info(&self, file: u64) -> VfsResult<VfsStreamInfo> {
        let (status, reply) = wire::stream_info(self.endpoint.id(), file).map_err(invoke_error)?;
        complete(status, reply.reply_status)?;
        Ok(reply.info)
    }
    pub fn read_dir(
        &self,
        _directory: &VfsPath,
        _cursor: u64,
        _name: &mut [u8],
    ) -> VfsResult<VfsDirReply> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }
    /// Convenience for one file; both path references are consumed even on error.
    pub fn open_path(&self, path: &[u8]) -> VfsResult<u64> {
        let root = self.root()?;
        let resolved = self.resolve(
            &VfsLookup {
                start: root,
                root,
                flags: KCOMP_VFS_LOOKUP_CROSS_MOUNTS | KCOMP_VFS_LOOKUP_FOLLOW_FINAL,
                max_symlinks: 0,
                encoding: KCOMP_VFS_ENCODING_BYTES,
                reserved: 0,
            },
            path,
        );
        let _ = self.release_path(&root);
        let found = resolved?;
        let opened = self.open(
            &VfsOpenRequest {
                path: found,
                access: KCOMP_VFS_ACCESS_READ,
                share: KCOMP_VFS_SHARE_READ,
                stream_kind: KCOMP_VFS_STREAM_DEFAULT,
                encoding: 0,
            },
            &[],
        );
        let _ = self.release_path(&found);
        opened
    }
}

fn invoke_error(error: InvokeError) -> VfsError {
    match error {
        InvokeError::Transport(errno) => VfsError::Transport(errno),
        InvokeError::Method(errno) => VfsError::Method(errno.code()),
        InvokeError::InvalidReply => VfsError::InvalidReply,
    }
}
fn complete(status: i32, reply: VfsReplyStatus) -> VfsResult<()> {
    if reply.reserved != 0
        || reply.domain > KCOMP_VFS_STATUS_NO_DATA_STREAM
        || (status == 0 && reply.domain != 0)
    {
        return Err(VfsError::InvalidReply);
    }
    if status == 0 {
        Ok(())
    } else if reply.domain != 0 {
        Err(VfsError::Domain {
            errno: status,
            detail: reply.domain,
        })
    } else {
        Err(VfsError::Method(status))
    }
}
fn copy_read(buffer: &mut [u8], output: &[u8], actual: u64) -> VfsResult<usize> {
    if actual > output.len() as u64 {
        return Err(VfsError::InvalidReply);
    }
    let actual = actual as usize;
    buffer[..actual].copy_from_slice(&output[..actual]);
    Ok(actual)
}
