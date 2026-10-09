//! VFS 只读契约声明。ABI 单一来源为 `abi/vfs.toml`；编码见
//! `docs/interfaces/vfs.md`。内部 Rust 对象不会跨组件边界。
//!
//! 只通过真实 Request/Reply IPC 消费服务；Path 与 Open 引用绑定调用 Task。
//! 不支持的目录枚举、写入和命名流返回 ENOTSUP。

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

pub mod codec;
/// Fixed IPC endpoint; never reconnect an old object to a new instance.
pub struct VfsBinding {
    pub endpoint: Endpoint<Vfs>,
}
impl VfsBinding {
    pub fn bind(endpoint: Endpoint<Vfs>) -> crate::Result<Self> {
        Ok(Self { endpoint })
    }
    fn invoke(&self, method: u32, args: &[u8], input: &[u8], output: &mut [u8]) -> VfsResult<()> {
        let status = crate::ipc::service::invoke(self.endpoint.id(), method, args, input, output)
            .map_err(VfsError::Transport)?;
        if output.len() < 8 || codec::u32_at(output, 4) != 0 {
            return Err(VfsError::InvalidReply);
        }
        let domain = codec::u32_at(output, 0);
        if domain > KCOMP_VFS_STATUS_NO_DATA_STREAM || (status == 0 && domain != 0) {
            return Err(VfsError::InvalidReply);
        }
        if status == 0 {
            Ok(())
        } else if domain != 0 {
            Err(VfsError::Domain {
                errno: status,
                detail: domain,
            })
        } else {
            Err(VfsError::Method(status))
        }
    }
    pub fn root(&self) -> VfsResult<VfsPath> {
        let mut out = [0; 40];
        self.invoke(KCOMP_VFS_METHOD_ROOT, &[], &[], &mut out)?;
        Ok(codec::path(&out[8..]))
    }
    /// Composition control only; drains opens and ends the owned server Task.
    pub fn shutdown(&self) -> VfsResult<()> {
        self.invoke(KCOMP_VFS_METHOD_SHUTDOWN, &[], &[], &mut [0; 8])
    }
    pub fn resolve(&self, request: &VfsLookup, path: &[u8]) -> VfsResult<VfsPath> {
        let mut args = [0; 80];
        codec::put_lookup(&mut args, request);
        let mut out = [0; 40];
        self.invoke(KCOMP_VFS_METHOD_RESOLVE, &args, path, &mut out)?;
        Ok(codec::path(&out[8..]))
    }
    pub fn retain_path(&self, path: &VfsPath) -> VfsResult<()> {
        self.path_operation(KCOMP_VFS_METHOD_RETAIN_PATH, path)
    }
    pub fn release_path(&self, path: &VfsPath) -> VfsResult<()> {
        self.path_operation(KCOMP_VFS_METHOD_RELEASE_PATH, path)
    }
    fn path_operation(&self, method: u32, path: &VfsPath) -> VfsResult<()> {
        let mut args = [0; 32];
        codec::put_path(&mut args, path);
        self.invoke(method, &args, &[], &mut [0; 8])
    }
    pub fn node_info(&self, path: &VfsPath) -> VfsResult<VfsNodeInfo> {
        let mut args = [0; 32];
        codec::put_path(&mut args, path);
        let mut out = [0; 32];
        self.invoke(KCOMP_VFS_METHOD_NODE_INFO, &args, &[], &mut out)?;
        Ok(VfsNodeInfo {
            kind: codec::u32_at(&out, 8),
            valid: codec::u32_at(&out, 12),
            link_count: codec::u64_at(&out, 16),
            name_encoding: codec::u32_at(&out, 24),
            case_rule: codec::u32_at(&out, 28),
        })
    }
    pub fn open(&self, request: &VfsOpenRequest, name: &[u8]) -> VfsResult<u64> {
        let mut args = [0; 48];
        codec::put_open(&mut args, request);
        let mut out = [0; 16];
        self.invoke(KCOMP_VFS_METHOD_OPEN, &args, name, &mut out)?;
        let id = codec::u64_at(&out, 8);
        if id == 0 {
            Err(VfsError::InvalidReply)
        } else {
            Ok(id)
        }
    }
    pub fn retain(&self, file: u64) -> VfsResult<()> {
        self.invoke(
            KCOMP_VFS_METHOD_RETAIN,
            &file.to_le_bytes(),
            &[],
            &mut [0; 8],
        )
    }
    pub fn close(&self, file: u64) -> VfsResult<()> {
        self.invoke(
            KCOMP_VFS_METHOD_CLOSE,
            &file.to_le_bytes(),
            &[],
            &mut [0; 8],
        )
    }
    pub fn set_position(&self, file: u64, offset: u64) -> VfsResult<()> {
        let mut args = [0; 16];
        codec::put64(&mut args, 0, file);
        codec::put64(&mut args, 8, offset);
        self.invoke(KCOMP_VFS_METHOD_SET_POSITION, &args, &[], &mut [0; 8])
    }
    fn read_method(&self, method: u32, args: &[u8], buffer: &mut [u8]) -> VfsResult<usize> {
        let count = buffer.len().min(512);
        let mut out = [0; 528];
        self.invoke(method, args, &[], &mut out[..16 + count])?;
        let actual = codec::u64_at(&out, 8);
        if actual > count as u64 {
            return Err(VfsError::InvalidReply);
        }
        let actual = actual as usize;
        buffer[..actual].copy_from_slice(&out[16..16 + actual]);
        Ok(actual)
    }
    pub fn read(&self, file: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        self.read_method(KCOMP_VFS_METHOD_READ, &file.to_le_bytes(), buffer)
    }
    pub fn read_at(&self, file: u64, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        let mut args = [0; 16];
        codec::put64(&mut args, 0, file);
        codec::put64(&mut args, 8, offset);
        self.read_method(KCOMP_VFS_METHOD_READ_AT, &args, buffer)
    }
    pub fn stream_info(&self, file: u64) -> VfsResult<VfsStreamInfo> {
        let mut out = [0; 64];
        self.invoke(
            KCOMP_VFS_METHOD_STREAM_INFO,
            &file.to_le_bytes(),
            &[],
            &mut out,
        )?;
        Ok(VfsStreamInfo {
            stream: VfsStream {
                fs: codec::u64_at(&out, 8),
                node: codec::u64_at(&out, 16),
                stream: codec::u64_at(&out, 24),
            },
            size: codec::u64_at(&out, 32),
            allocated_size: codec::u64_at(&out, 40),
            valid_data_length: codec::u64_at(&out, 48),
            valid: codec::u32_at(&out, 56),
            reserved: codec::u32_at(&out, 60),
        })
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
