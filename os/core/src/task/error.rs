//! TaskTable 操作的错误类型。

#[derive(Debug, PartialEq, Eq)]
pub enum TaskError {
    /// 同一 id 已存在（create/insert 撞号，真相上不应发生：id 由 Core 单调分配）。
    AlreadyExists,
    /// 表内没有该 id（存在性验证失败：ID 可被猜测，查无此人才是真相）。
    NotFound,
    /// 内存不足。
    NoMemory,
}
