//! 服务语义地址复用 SDK 值；接口配置仍是组件私有类型。

pub use kcomp_sdk::network::{AddressFamily, BindAddress, InterfaceId, IpAddress, SocketAddress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceAddress {
    pub ip: IpAddress,
    pub prefix_len: u8,
}
