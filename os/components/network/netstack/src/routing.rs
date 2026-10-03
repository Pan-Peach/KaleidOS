//! 原生出接口 / 源地址选择；smoltcp 的每接口路由表由此同步。

use crate::Result;
use crate::address::{InterfaceAddress, InterfaceId, IpAddress};
use crate::backend::DeviceStack;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteEntry {
    /// 网络前缀，登记时须归一化；prefix_len = 0 可表示默认路由。
    pub destination: InterfaceAddress,
    pub interface: InterfaceId,
    pub next_hop: Option<IpAddress>,
    pub metric: u32,
}

pub struct RouteDecision {
    pub interface: InterfaceId,
    pub source: IpAddress,
    pub next_hop: Option<IpAddress>,
    pub is_local: bool,
}

pub struct RoutingTable<'a> {
    pub entries: &'a mut [Option<RouteEntry>],
}

impl RoutingTable<'_> {
    pub fn select(
        &self,
        _devices: &[DeviceStack<'_>],
        _destination: IpAddress,
        _source: Option<IpAddress>,
        _interface: Option<InterfaceId>,
    ) -> Result<RouteDecision> {
        todo!("识别本地投递，按地址族 / 最长前缀 / metric 选接口和源地址；无路由报错")
    }
}
