//! Namespace：挂载关系与路径遍历，不拥有 fd / HANDLE / 进程 cwd。
//! 所有记录均应放在 VFS 实例状态中，不使用可变全局表。

use crate::name::PathInput;
use crate::provider::{FsInstanceId, NodeRef};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountId(pub u64);

/// 路径位置草案：挂载位置、目录项与底层节点是不同身份。
/// TODO: 引用保活、目录项代次与 rename 后的 parent 关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathRef {
    pub mount: MountId,
    pub entry: u64,
    pub node: NodeRef,
}

pub struct Mount {
    pub id: MountId,
    /// None 表示 namespace 根；其余挂载指向父 namespace 中的位置。
    pub at: Option<PathRef>,
    pub root: NodeRef,
}

/// 遍历范围草案。Root 允许在 root 下解析绝对 / 相对路径；
/// BeneathStart 还限制在起点之下，不能靠 .. / symlink / mount 跳出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupBoundary {
    Root,
    BeneathStart,
}

pub struct LookupOptions {
    pub boundary: LookupBoundary,
    pub follow_final_symlink: bool,
    pub cross_mounts: bool,
    pub max_symlinks: u32,
}

/// 每次遍历显式传入起点和边界；personality 自己保存 cwd / preopened dir。
/// TODO: 在实际 symlink / mount 遍历中检查边界，不能只清洗路径字符串。
pub struct LookupContext {
    pub start: PathRef,
    pub root: PathRef,
    pub options: LookupOptions,
}

/// 表存储由实例运行时提供；暂不决定堆分配器、容量或缓存算法。
/// TODO: 目录项记录 / 名字保活、mount / entry 发号与锁，由人类实现。
pub struct Namespace<'a> {
    pub mounts: &'a mut [Option<Mount>],
}

impl Namespace<'_> {
    /// TODO: 记录挂载关系；不能把 namespace attach 等同于再次 mount provider。
    pub fn attach(&mut self, _at: Option<PathRef>, _root: NodeRef) -> Result<MountId> {
        Err(Error::Unsupported)
    }

    /// TODO: busy / detach 语义与路径引用保活；detach 不等于 provider unmount。
    pub fn detach(&mut self, _mount: MountId) -> Result<()> {
        Err(Error::Unsupported)
    }

    /// TODO: 引用表与 mount / entry / node 保活；复制 PathRef 本身不增加用户引用。
    pub fn retain(&mut self, _path: PathRef) -> Result<()> {
        Err(Error::Unsupported)
    }

    pub fn release(&mut self, _path: PathRef) -> Result<()> {
        Err(Error::Unsupported)
    }

    /// TODO: 逐段解析并保活结果；检查目录、symlink 次数、实际 mount 跨越与边界。
    /// 不得静默替换不可表示字符，也不得统一转小写。
    pub fn resolve(&self, _context: &LookupContext, _path: PathInput<'_>) -> Result<PathRef> {
        Err(Error::Unsupported)
    }

    /// provider 失败后这些挂载 / 路径逻辑失效；不能把它们重绑到新 incarnation。
    /// TODO: 和 FileService 失效在同一实例生命周期中协调。
    pub fn invalidate_provider(&mut self, _instance: FsInstanceId) -> Result<()> {
        Err(Error::Unsupported)
    }
}
