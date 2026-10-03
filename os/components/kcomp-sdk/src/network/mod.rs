//! 网络服务契约草案；使用示例见 `docs/interfaces/network.md`。
//!
//! consumer 只依赖 SDK，不依赖 netstack / smoltcp。Rust 值只在各自镜像内使用，
//! 边界为 `abi/network.toml` 的 C table / LE frame。bind 暂返回 ENOTSUP；
//! 其他操作仍为 todo!()，不能发布或调用可用的网络服务。

mod client;
mod provider;
mod types;

pub use client::{InetSocket, NetworkBinding, Subscription, TcpSocket, UdpSocket};
pub use provider::{NetworkProvider, NetworkService};
pub use types::*;

use crate::abi::InterfaceKind;
use crate::endpoint::Contract;
use crate::errno::Errno;
use crate::generated::network::{KCOMP_NETWORK_ABI, KCOMP_NETWORK_CONTRACT};

pub struct Network;

impl Contract for Network {
    const ID: u64 = KCOMP_NETWORK_CONTRACT;
    const ABI: u64 = KCOMP_NETWORK_ABI;
    const KIND: InterfaceKind = InterfaceKind::Service;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkError {
    Transport(Errno),
    /// provider 同步点竞争，未执行操作；调用方让出执行机会后重试，不能等 socket 事件。
    Busy,
    /// 保留 provider 的负 errno；应用错误翻译由 personality 负责。
    Method(i32),
    InvalidReply,
}

pub type NetworkResult<T> = core::result::Result<T, NetworkError>;

#[cfg(test)]
mod examples;
