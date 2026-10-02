//! 数据流语义；物理 extent、块映射和 ADS 索引结构属于 FS provider。

use crate::name::NameRef;
use crate::provider::NodeRef;

/// 流 token 在节点 / FS 实例范围内有效；默认流也有身份，不约定 token = 0。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRef {
    pub node: NodeRef,
    pub token: u64,
}

/// personality 已解析流选择；VFS 不把冒号自动解释为 ADS。
/// 目录可以有命名流，但不因此具备默认数据流。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSelector<'a> {
    Default,
    Named(NameRef<'a>),
}

/// provider 返回的快照；文件长度属于流，不属于统一 inode。
/// None 表示该属性没有可用语义，不能当成 0 或从其他字段推算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub stream: StreamRef,
    pub size: u64,
    pub allocated_size: Option<u64>,
    pub valid_data_length: Option<u64>,
}
