//! 名字表示与匹配提示。personality 解析自己的路径语法，provider 决定原生匹配。
//! 不要求名字是 UTF-8；转换失败须显式报错，不能丢字符或统一转小写。

/// 单个目录项 / 流的名字；合法字符、长度与编码支持由 provider 检查。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameRef<'a> {
    Bytes(&'a [u8]),
    Utf16(&'a [u16]),
}

/// 完整路径输入，与单段 NameRef 分开。分隔符等语法尚待 personality 契约定稿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathInput<'a> {
    Bytes(&'a [u8]),
    Utf16(&'a [u16]),
}

/// 目录枚举使用调用者提供的缓冲区；返回名字借用此缓冲区。
/// TODO: 定下跨 ABI 的编码 / 长度表示，不传递这些 Rust 引用。
pub enum NameBuffer<'a> {
    Bytes(&'a mut [u8]),
    Utf16(&'a mut [u16]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameEncoding {
    Bytes,
    Utf16,
}

/// 只是匹配提示；折叠 / 排序算法仍由 provider 实现，不能据此在 VFS 中 lower()。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseRule {
    Sensitive,
    Insensitive,
    ProviderDefined,
}

/// provider 按卷 / 目录返回；不是每个普通文件上的 CASE_INSENSITIVE 标志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameRules {
    pub encoding: NameEncoding,
    pub case: CaseRule,
}
