//! VFS 只读契约声明。ABI 单一来源为 `abi/vfs.toml`；编码见
//! `docs/interfaces/vfs.md`。内部 Rust 对象不会跨组件边界。
//!
//! 当前提供 Contract / C table / Rust API 形状，尚无 Direct/Gate 适配器。
//! Binding 的所有入口显式返回 ENOTSUP，不调用 provider；不能发布可用服务。
//! 适配器实现须保持两种 transport 同义，由 Core 在 bind 时选择。

use crate::abi::InterfaceKind;
use crate::endpoint::{Contract, Endpoint};
use crate::errno::Errno;

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

/// 组件内 provider 接口草案；仅跨 C table / flat frame，不传递此 trait object。
/// read_dir 返回的 name_len 必须不超过 name.len()；错误时不返回截断条目。
/// read_dir 的 ENOBUFS 映射为 VfsError::BufferTooSmall，适配器保留 required。
pub trait VfsProvider {
    fn root(&self) -> VfsResult<VfsPath>;
    fn retain_path(&self, path: &VfsPath) -> VfsResult<()>;
    fn release_path(&self, path: &VfsPath) -> VfsResult<()>;
    fn resolve(&self, request: &VfsLookup, path: &[u8]) -> VfsResult<VfsPath>;
    fn node_info(&self, path: &VfsPath) -> VfsResult<VfsNodeInfo>;
    fn read_dir(&self, directory: &VfsPath, cursor: u64, name: &mut [u8])
    -> VfsResult<VfsDirReply>;
    fn open(&self, request: &VfsOpenRequest, stream_name: &[u8]) -> VfsResult<u64>;
    fn retain(&self, file: u64) -> VfsResult<()>;
    fn read(&self, file: u64, buffer: &mut [u8]) -> VfsResult<usize>;
    fn read_at(&self, file: u64, offset: u64, buffer: &mut [u8]) -> VfsResult<usize>;
    fn set_position(&self, file: u64, offset: u64) -> VfsResult<()>;
    fn stream_info(&self, file: u64) -> VfsResult<VfsStreamInfo>;
    fn close(&self, file: u64) -> VfsResult<()>;
}

/// consumer 的绑定形状；不能缓存失效后重新发现的同名 endpoint 来重定向旧引用。
/// TODO: 持有 Core bind 选定的后端，并实现 C table / flat frame 编解码。
pub struct VfsBinding {
    pub endpoint: Endpoint<Vfs>,
}

impl VfsBinding {
    pub fn bind(_endpoint: Endpoint<Vfs>) -> crate::errno::Result<Self> {
        Err(Errno::ENOTSUP)
    }

    pub fn root(&self) -> VfsResult<VfsPath> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn retain_path(&self, _path: &VfsPath) -> VfsResult<()> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn release_path(&self, _path: &VfsPath) -> VfsResult<()> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn resolve(&self, _request: &VfsLookup, _path: &[u8]) -> VfsResult<VfsPath> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn node_info(&self, _path: &VfsPath) -> VfsResult<VfsNodeInfo> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn read_dir(
        &self,
        _directory: &VfsPath,
        _cursor: u64,
        _name: &mut [u8],
    ) -> VfsResult<VfsDirReply> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn open(&self, _request: &VfsOpenRequest, _stream_name: &[u8]) -> VfsResult<u64> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn retain(&self, _file: u64) -> VfsResult<()> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn read(&self, _file: u64, _buffer: &mut [u8]) -> VfsResult<usize> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn read_at(&self, _file: u64, _offset: u64, _buffer: &mut [u8]) -> VfsResult<usize> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn set_position(&self, _file: u64, _offset: u64) -> VfsResult<()> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn stream_info(&self, _file: u64) -> VfsResult<VfsStreamInfo> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }

    pub fn close(&self, _file: u64) -> VfsResult<()> {
        Err(VfsError::Method(Errno::ENOTSUP.code()))
    }
}
