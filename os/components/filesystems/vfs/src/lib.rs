//! VFS 组件骨架：Namespace + File service，边界见
//! `docs/interfaces/filesystem.md`；当前落点见 `docs/modules/vfs.md`。
//!
//! 这里只放内部类型草案与未实现入口，供人类逐步手写实现。
//! Rust 类型不跨组件边界；服务 ABI 定稿后才进入 `abi/*.toml` 与 SDK。
//! 当前 create 返回 ENOTSUP，不发布 endpoint，不创建任务。

#![no_std]

#[cfg(test)]
extern crate std;

pub mod file;
pub mod name;
pub mod namespace;
pub mod provider;
pub mod stream;

#[cfg(not(test))]
mod runtime;

/// 组件内部的错误草案；不是 C ABI，也没有确定错误码映射。
/// 共享冲突、删除等待与权限拒绝必须保留区别，供 personality 翻译。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    UnsupportedName,
    InvalidName,
    NotFound,
    NotDirectory,
    NoDataStream,
    TooManySymlinks,
    OutsideRoot,
    BufferTooSmall { required: usize },
    StaleReference,
    PermissionDenied,
    SharingViolation,
    DeletePending,
    Provider(kcomp_sdk::endpoint::InvokeError),
}

pub type Result<T> = core::result::Result<T, Error>;
