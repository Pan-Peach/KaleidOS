//! Handle 校验与生命周期错误。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    /// token 指向不存在的 slot。
    Invalid,
    /// token 的 generation 不是该 slot 当前 generation。
    Stale,
    /// caller 不是 slot owner。
    WrongOwner,
    /// slot 仍存在，但资源已经被 Core revoke。
    Revoked,
    /// 对已释放资源重复 release。
    AlreadyReleased,
}
