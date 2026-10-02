//! File service：打开实例、游标与多个请求之间的协调。
//! open 之后按对象操作，不重新解析路径；fd / HANDLE 表属于 personality。

use crate::namespace::PathRef;
use crate::provider::{AccessIntent, FsInstanceId, ProviderHandle};
use crate::stream::{StreamRef, StreamSelector};
use crate::{Error, Result};

/// VFS 的打开实例身份；不是 POSIX fd 或 NT HANDLE。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFileId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareMode {
    pub read: bool,
    pub write: bool,
    pub delete: bool,
}

/// personality 明确选择访问与共享语义；不在这里存 O_* / FILE_* 位编码。
pub struct OpenRequest<'a> {
    pub stream: StreamSelector<'a>,
    pub access: AccessIntent,
    pub share: ShareMode,
}

/// 仅统计 VFS 持有的引用。复制句柄不增加独立 open 数；link_count 不在这里。
/// provider 还可能持有缓存引用，全部归零也不等于可以回收物理 backing。
pub struct OpenRefs {
    pub handles: u32,
    pub inflight_io: u32,
    pub mappings: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenState {
    Live,
    /// 用户句柄为零：拒绝 retain / 新用户 I/O，等待已有请求后执行 cleanup。
    Handleless,
    /// cleanup 已完成；映射 / 内部 I/O 可以继续保活，最终 close 仍待引用排空。
    Cleaned,
    /// 逻辑失效，不继续调用失败 provider；不承诺物理清理或回收。
    ProviderFailed,
}

/// 每次 open 创建独立对象；retain 共享游标与此对象的访问 / 共享模式。
/// TODO: token 发号不复用、引用溢出、并发 close 与 cleanup / 最终 close 的唯一性。
pub struct OpenFile {
    pub id: OpenFileId,
    /// 保留打开时的路径位置供删除协调；保活此引用不代表目录项仍然存在。
    pub opened_at: PathRef,
    pub stream: StreamRef,
    pub provider_handle: ProviderHandle,
    pub position: u64,
    pub access: AccessIntent,
    pub share: ShareMode,
    pub refs: OpenRefs,
    pub state: OpenState,
}

/// 从同一流上的独立 open 推导的计数；retain / close 单个复制句柄不更新它。
/// TODO: 双向检查（新 access 对旧 share，旧 access 对新 share），和 open / cleanup
/// 在同一协调边界中更新；不能把预检与提交分成无锁的两步。
pub struct ShareState {
    pub opens: u32,
    pub readers: u32,
    pub writers: u32,
    pub deleters: u32,
    pub share_read: u32,
    pub share_write: u32,
    pub share_delete: u32,
}

pub struct StreamState {
    pub stream: StreamRef,
    pub shares: ShareState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteId(pub u64);

/// 删除名字与删除命名流不是同一个操作；普通 inode 上一个布尔位不足以表达。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTarget {
    DirectoryEntry(PathRef),
    NamedStream(StreamRef),
}

/// 内部请求草案；POSIX unlink / NT disposition 的具体冲突与触发规则尚未定稿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTiming {
    RemoveName,
    WhenUnused,
}

pub struct DeleteRequest {
    pub target: DeleteTarget,
    pub timing: DeleteTiming,
    pub requested_by: Option<OpenFileId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteState {
    Pending,
    Committed,
    Cancelled,
    /// provider 失败可能使提交结果未知；不能自动声称已取消或已回收。
    ProviderFailed,
}

pub struct PendingDelete {
    pub id: DeleteId,
    pub request: DeleteRequest,
    pub state: DeleteState,
}

/// 实例拥有表的存储与同步；借用槽位仅给出状态形状，不实现分配 / 发号算法。
pub struct FileService<'a> {
    pub opens: &'a mut [Option<OpenFile>],
    pub streams: &'a mut [Option<StreamState>],
    pub deletes: &'a mut [Option<PendingDelete>],
}

impl FileService<'_> {
    /// 第一阶段只实现只读数据流；不支持的访问类别必须拒绝，不能忽略。
    /// TODO: 路径 / 流保活，权限、delete-pending 与 share 检查，provider open 回滚。
    /// 创建 / 截断须 provider 原子操作，不能用 lookup + create 拼出排他创建。
    pub fn open(&mut self, _path: PathRef, _request: &OpenRequest<'_>) -> Result<OpenFileId> {
        Err(Error::Unsupported)
    }

    /// TODO: 复制用户引用，共享同一打开实例；不再次调用 provider open。
    pub fn retain(&mut self, _file: OpenFileId) -> Result<()> {
        Err(Error::Unsupported)
    }

    /// TODO: 顺序读取与游标更新必须协调，同一打开实例的引用共享游标。
    pub fn read(&mut self, _file: OpenFileId, _buffer: &mut [u8]) -> Result<usize> {
        Err(Error::Unsupported)
    }

    /// TODO: 显式偏移读取不改变游标；当前 provider ABI 尚不支持。
    pub fn read_at(
        &mut self,
        _file: OpenFileId,
        _offset: u64,
        _buffer: &mut [u8],
    ) -> Result<usize> {
        Err(Error::Unsupported)
    }

    pub fn set_position(&mut self, _file: OpenFileId, _offset: u64) -> Result<()> {
        Err(Error::Unsupported)
    }

    /// TODO: 关闭引用与最终释放分开；在途 I/O / 映射可能继续持有对象。
    pub fn close(&mut self, _file: OpenFileId) -> Result<()> {
        Err(Error::Unsupported)
    }

    /// TODO: 授权、目标保活与 provider 原子删除；文件删除须检查所有流上的 share。
    /// 当前没有删除 ABI；WhenUnused 的具体计数范围必须先与 provider 定稿。
    pub fn request_delete(&mut self, _request: &DeleteRequest) -> Result<DeleteId> {
        Err(Error::Unsupported)
    }

    /// TODO: 失效该 incarnation 的打开 / 流状态，返回 StaleReference，保留失败驻留。
    pub fn invalidate_provider(&mut self, _instance: FsInstanceId) -> Result<()> {
        Err(Error::Unsupported)
    }
}
