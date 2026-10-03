//! 基于 smoltcp 的网络栈组件骨架，手写入口见 docs/modules/netstack.md。
//!
//! 原生 socket / 端口 / 路由 → 每接口 smoltcp backend → NetDevice。
//! 对外契约 / SDK 见 kcomp_sdk::network；本镜像的 Stack / Context 不向调用方暴露。
//! 协议状态与 socket 语义归本实例，task / timer / 生命周期真相仍归 Core。
//! 未实现入口使用 todo!()；当前 create 返回 ENOTSUP，不进入这些入口。

#![no_std]

pub mod address;
pub mod backend;
pub mod clock;
pub mod device;
pub mod port;
pub mod routing;
pub mod service;
pub mod socket;
pub mod stack;
pub mod tcp;
pub mod udp;
pub mod worker;

#[cfg(not(test))]
mod runtime;

pub use socket::{Attempt, InetSocket, SocketEvents, SocketId};
pub use stack::{Stack, StackConfig, StackStorage};

/// 内部错误草案，待网络服务 C ABI 定稿后映射；不是 Rust enum ABI。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Busy,
    Unsupported,
    InvalidAddress,
    InvalidState,
    NoRoute,
    AddressInUse,
    InvalidSocket,
    WrongSocketType,
    SocketLimit,
    NotConnected,
    ConnectionRefused,
    ConnectionReset,
    TimedOut,
    MessageTooLarge,
    BufferTooSmall { required: usize },
    DeviceFailed,
    ClockUnavailable,
    TimeOverflow,
}

pub type Result<T> = core::result::Result<T, Error>;
