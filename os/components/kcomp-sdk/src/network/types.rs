//! SDK / provider 各自编译的语义值；不以 Rust layout 跨组件传递。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
    Ipv4,
    Ipv6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpAddress {
    Ipv4([u8; 4]),
    Ipv6([u8; 16]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketAddress {
    pub ip: IpAddress,
    pub port: u16,
}

/// None 是本地通配地址；port=0 由网络服务分配临时端口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindAddress {
    pub ip: Option<IpAddress>,
    pub port: u16,
}

/// 只在同一服务实例内有效；不是 Core endpoint、权限或进程描述符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportProtocol {
    Tcp,
    Udp,
}

/// Pending 没有登记请求，也不保留调用方 buffer。发送 Ready 只表示入队。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt<T> {
    Ready(T),
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketInfo {
    pub id: SocketId,
    pub protocol: TransportProtocol,
    pub family: AddressFamily,
    pub local: Option<BindAddress>,
    pub interface: Option<InterfaceId>,
}

/// 状态提示；查询不 poll，不清除错误。revision 也覆盖连接完成 / 失败 / FIN。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketEvents {
    pub revision: u64,
    pub receive_available: bool,
    pub send_capacity: bool,
    pub incoming_connection: bool,
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionPhase {
    Idle,
    Connecting,
    Established,
    Listening,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionStatus {
    pub phase: ConnectionPhase,
    pub local: Option<SocketAddress>,
    pub peer: Option<SocketAddress>,
    pub send_finished: bool,
    pub receive_ended: bool,
    /// 持久的负 errno，读取不清除；不是某个 personality 的 SO_ERROR。
    pub failure: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamReceive {
    Bytes(usize),
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatagramInfo {
    pub source: SocketAddress,
    pub destination: SocketAddress,
    pub interface: InterfaceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatagramReadMode {
    /// 复制前缀，始终保留整包。
    Peek,
    /// 足够大才复制并消费；否则 copied=0、consumed=false，整包保留。
    Whole,
    /// 复制前缀并消费整包。
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceivedDatagram {
    pub info: DatagramInfo,
    pub copied: usize,
    pub original_len: usize,
    pub consumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionId(pub u64);

pub struct SocketSubscription {
    pub id: SubscriptionId,
    /// 登记与读取在同一同步点完成；收到快照后仍须重试具体操作再决定是否 park。
    pub current: SocketEvents,
}
