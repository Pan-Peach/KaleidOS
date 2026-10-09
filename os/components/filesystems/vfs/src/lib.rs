//! VFS 只读对象模块与独立 IPC 服务入口，边界见
//! `docs/interfaces/filesystem.md`；当前落点见 `docs/modules/vfs.md`。
//!
//! 一个 Namespace/OpenFile 同时使用 LocalFs 与 RemoteFs。
//! Rust 类型不跨组件边界；外部协议由 `abi/vfs.toml` 定义。

#![cfg_attr(target_os = "none", no_std)]
extern crate alloc;

pub mod file;
pub mod local;
pub mod name;
pub mod namespace;
pub mod provider;
pub mod remote;
mod service;

#[cfg(not(test))]
mod runtime;

pub use kcomp_sdk::Errno as Error;
pub type Result<T> = core::result::Result<T, Error>;

#[cfg(test)]
mod tests;
