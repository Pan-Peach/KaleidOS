//! FS provider 接入点。消费 SDK typed binding，调用机制由 Core 在 bind 时选择。
//! `filesystem` 已有 root / 单段 lookup / node_info（FatFs 已实现，littlefs 不支持）。
//! 本适配器仍为占位；目录枚举、read_at、节点引用保活等尚无 provider 契约。

use kcomp_sdk::endpoint::Endpoint;
use kcomp_sdk::filesystem::FileSystem;
use kcomp_sdk::filesystem::client::FileSystemBinding;

use crate::name::{NameBuffer, NameRef, NameRules, PathInput};
use crate::stream::{StreamInfo, StreamRef, StreamSelector};
use crate::{Error, Result};

/// VFS 内部的 FS incarnation；namespace attach 不创建新 incarnation。
/// TODO: 在本 VFS 实例内发号不复用；provider 重启 / 重新 mount 后旧引用失效。
/// 全部内部 ID 还受 VFS 组件实例的有效期约束，不是跨重启的全局标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsInstanceId(pub u64);

/// 节点引用草案：身份与路径、打开实例分离。
/// TODO: 对接 FS ABI 的挂载期 token，并在 provider 重新挂载后失效旧引用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeRef {
    pub instance: FsInstanceId,
    pub token: u64,
}

/// 用于共享协调的访问类别；不是完整原生权限位，也不是已获授权。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessIntent {
    pub read: bool,
    pub write: bool,
    pub delete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityModel {
    PosixAcl,
    Windows,
    ProviderDefined,
}

/// 原生权限快照草案。POSIX uid/gid 不是 Core ComponentId。
/// 带 ACL 的 provider 不能降级为 PosixMode；Native token 只在对应节点范围内有效。
/// TODO: 定下原生 descriptor 查询、调用者凭据和授权契约，不自动互译 POSIX / NT。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityDescriptor {
    Unsupported,
    PosixMode { uid: u32, gid: u32, mode: u16 },
    Native { model: SecurityModel, token: u64 },
}

/// 元数据真相来自 provider；这只是查询快照，不在 VFS 中重复维护 link_count。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeInfo {
    pub node: NodeRef,
    pub kind: NodeKind,
    pub link_count: Option<u64>,
    pub directory_rules: Option<NameRules>,
    pub security: SecurityDescriptor,
}

/// 只用于选择请求；每次操作仍须处理 Unsupported，不能据此承诺成功。
pub struct ProviderFeatures {
    pub named_streams: bool,
    pub read_at: bool,
}

/// provider 的打开 token，与 node / stream / VFS OpenFileId 都不是同一身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderHandle {
    pub instance: FsInstanceId,
    pub token: u64,
}

/// 不透明的枚举位置；不是数组下标，也不承诺并发目录修改下的快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryCursor(pub u64);

pub struct DirectoryEntry<'a> {
    pub name: NameRef<'a>,
    pub node: NodeRef,
    pub next: DirectoryCursor,
}

/// 已绑定 provider 的记录形状；发现 / bind / 表管理均待实现。
/// 多条 namespace 入口应共享这个 FS 实例，而不是再次创建 provider。
pub struct FsProvider {
    pub instance: FsInstanceId,
    pub endpoint: Endpoint<FileSystem>,
    pub binding: FileSystemBinding,
}

impl FsProvider {
    // root / lookup / node_info 已有 binding 入口，尚未适配到 VFS 类型；其余契约待补齐。
    // 不降级为字符串拼路径、重复顺序读取或临时打开来伪造节点 / 偏移读取。

    pub fn features(&self) -> Result<ProviderFeatures> {
        Err(Error::Unsupported)
    }

    pub fn root(&self) -> Result<NodeRef> {
        Err(Error::Unsupported)
    }

    /// provider 按 parent 的原生规则匹配一个名字；VFS 不自行比较 / 折叠。
    pub fn lookup(&self, _parent: NodeRef, _name: NameRef<'_>) -> Result<NodeRef> {
        Err(Error::Unsupported)
    }

    pub fn node_info(&self, _node: NodeRef) -> Result<NodeInfo> {
        Err(Error::Unsupported)
    }

    /// TODO: namespace / open 的节点保活与 provider 原生引用，不与 link_count 混用。
    pub fn retain_node(&self, _node: NodeRef) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn release_node(&self, _node: NodeRef) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn read_link<'a>(&self, _node: NodeRef, _target: NameBuffer<'a>) -> Result<PathInput<'a>> {
        Err(Error::Unsupported)
    }

    /// None 是枚举结束；名字缓冲区不足应返回 BufferTooSmall，不截断。
    /// TODO: 规定起始 cursor、修改后的 cursor 有效性与返回 token 的保活。
    pub fn read_dir<'a>(
        &self,
        _directory: NodeRef,
        _cursor: DirectoryCursor,
        _name: NameBuffer<'a>,
    ) -> Result<Option<DirectoryEntry<'a>>> {
        Err(Error::Unsupported)
    }

    pub fn resolve_stream(
        &self,
        _node: NodeRef,
        _selector: StreamSelector<'_>,
    ) -> Result<StreamRef> {
        Err(Error::Unsupported)
    }

    pub fn stream_info(&self, _stream: StreamRef) -> Result<StreamInfo> {
        Err(Error::Unsupported)
    }

    /// TODO: provider 原生授权与 VFS share 检查的提交 / 回滚边界，不能只预检后放锁。
    pub fn open_stream(&self, _stream: StreamRef, _access: AccessIntent) -> Result<ProviderHandle> {
        Err(Error::Unsupported)
    }

    pub fn read_at(
        &self,
        _handle: ProviderHandle,
        _offset: u64,
        _buffer: &mut [u8],
    ) -> Result<usize> {
        Err(Error::Unsupported)
    }

    /// 最后一个用户句柄关闭后的清理，与最终释放 provider handle 分开。
    /// TODO: 原生 provider 可能仍持有缓存 / 映射引用；现有 ABI 只有 close。
    pub fn cleanup(&self, _handle: ProviderHandle) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn close(&self, _handle: ProviderHandle) -> Result<()> {
        Err(Error::Unsupported)
    }
}
