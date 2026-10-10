//! TaskTable 操作的错误类型。

#[derive(Debug, PartialEq, Eq)]
pub enum TaskError {
    /// 同一 id 已存在（create/insert 撞号，真相上不应发生：id 由 Core 单调分配）。
    AlreadyExists,
    /// 表内没有该 id（存在性验证失败：ID 可被猜测，查无此人才是真相）。
    NotFound,
    /// 内存不足。
    NoMemory,
    InvalidOutput,
    /// 状态机非法转换（如 Runnable 再 start、Exited 终态再推进）。
    InvalidTransition,
    /// 请求创建任务的组件不存在（requester 未声明）。
    RequesterNotFound,
    /// 请求创建任务的组件尚未 Ready（只有 Ready 组件能创建任务）。
    RequesterNotReady,
    /// 请求者不是任务 owner。
    WrongOwner,
    /// entry 不落在 requester 组件的装载镜像内（越界指针一律拒绝）。
    EntryOutOfImage,
}
