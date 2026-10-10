//! 组件内网络端点的共有能力；fd / HANDLE、阻塞规则和错误码映射属于 personality。
//! InetSocket 只描述信息与事件，不强迫 TCP listener / connection / UDP 共用操作表。

use smoltcp::storage::PacketBuffer;

use crate::backend::DeviceStack;
use crate::port::{PortRegistry, PortReservation};
use crate::routing::RoutingTable;
use crate::tcp::{TcpConnection, TcpListener};
use crate::udp::{DatagramInfo, UdpEndpoint};

pub use kcomp_sdk::network::{
    Attempt, SocketEvents, SocketId, SocketInfo, SocketSubscription, SubscriptionId,
    TransportProtocol,
};

/// provider 内部静态分发；对外契约是 SDK NetworkProvider 与未来 IPC 协议。
pub trait InetSocket {
    fn info(&self) -> SocketInfo;
    fn events(&self) -> SocketEvents;
}

pub struct SocketCommon {
    pub info: SocketInfo,
    pub port: Option<PortReservation>,
    pub events: SocketEvents,
    /// 逻辑退役后禁止新访问，协议清理完成前仍保留对象 / buffers / 端口。
    pub retired: bool,
}

/// 固定表存储，借用实例 backing；无需 Box<dyn InetSocket> 或共享 Rust runtime。
#[allow(clippy::large_enum_variant)] // connection 内联保留尚未 attach 的私有 smoltcp socket。
pub enum SocketRecord<'a> {
    TcpConnection(TcpConnection<'a>),
    TcpListener(TcpListener<'a>),
    UdpEndpoint(UdpEndpoint<'a>),
}

impl InetSocket for SocketRecord<'_> {
    fn info(&self) -> SocketInfo {
        todo!("静态分发到具体端点的信息快照")
    }

    fn events(&self) -> SocketEvents {
        todo!("静态分发已发布事件；不在查询 / 等待条件中执行 poll")
    }
}

/// 在实例同步点内分开借用记录和共享资源；不能 park、外调或重入 Stack。
pub struct SocketContext<'s, 'a> {
    pub devices: &'s mut [DeviceStack<'a>],
    pub ports: &'s mut PortRegistry<'a>,
    pub routes: &'s RoutingTable<'a>,
    /// 本地发送先有界入队，worker 再匹配其他 endpoint，避免借用整个 socket 表。
    pub local_datagrams: &'s mut PacketBuffer<'a, DatagramInfo>,
}

pub struct SocketObserver {
    pub id: SubscriptionId,
    pub socket: SocketId,
    pub task: u32,
}
