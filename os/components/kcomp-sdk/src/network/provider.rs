//! provider 的本镜像适配面；C ABI table 与 Gate 分发仍待手写。

use crate::endpoint::Endpoint;
use crate::frame::Call;

use super::*;

/// 业务 facade：只接收值、对象 ID 和本次调用借用的 payload。
/// 方法不 park、不轮询网卡；同一语义同时供 Direct / Gate 适配器使用。
pub trait NetworkProvider {
    fn tcp_open(&self, family: AddressFamily) -> NetworkResult<SocketId>;
    fn udp_open(&self, family: AddressFamily) -> NetworkResult<SocketId>;
    fn socket_bind(&self, socket: SocketId, local: BindAddress) -> NetworkResult<()>;
    fn tcp_connect(&self, socket: SocketId, peer: SocketAddress) -> NetworkResult<()>;
    fn tcp_listen(&self, socket: SocketId, pending_limit: u32) -> NetworkResult<()>;
    fn tcp_accept(&self, listener: SocketId) -> NetworkResult<Attempt<SocketId>>;
    fn tcp_status(&self, socket: SocketId) -> NetworkResult<ConnectionStatus>;
    fn tcp_send(&self, socket: SocketId, data: &[u8]) -> NetworkResult<Attempt<usize>>;
    fn tcp_receive(
        &self,
        socket: SocketId,
        buffer: &mut [u8],
    ) -> NetworkResult<Attempt<StreamReceive>>;
    fn tcp_finish_send(&self, socket: SocketId) -> NetworkResult<()>;
    fn tcp_abort(&self, socket: SocketId) -> NetworkResult<()>;
    fn udp_send_to(
        &self,
        socket: SocketId,
        peer: SocketAddress,
        data: &[u8],
    ) -> NetworkResult<Attempt<()>>;
    fn udp_receive(
        &self,
        socket: SocketId,
        buffer: &mut [u8],
        mode: DatagramReadMode,
    ) -> NetworkResult<Attempt<ReceivedDatagram>>;
    fn udp_set_receive_peer(
        &self,
        socket: SocketId,
        peer: Option<SocketAddress>,
    ) -> NetworkResult<()>;
    fn socket_info(&self, socket: SocketId) -> NetworkResult<SocketInfo>;
    fn socket_events(&self, socket: SocketId) -> NetworkResult<SocketEvents>;
    fn socket_subscribe(&self, socket: SocketId, task: u32) -> NetworkResult<SocketSubscription>;
    fn socket_unsubscribe(&self, subscription: SubscriptionId) -> NetworkResult<()>;
    fn socket_release(&self, socket: SocketId) -> NetworkResult<()>;
}

/// 单态化为私有 C adapters；不把 P / trait object 当成组件 ABI。
pub struct NetworkService<P> {
    pub provider: P,
}

impl<P: NetworkProvider + 'static> NetworkService<P> {
    pub fn publish(&'static self, _port_name: &str) -> crate::errno::Result<Endpoint<Network>> {
        todo!("完成 C adapters / Gate dispatcher 后，发布 NetworkApi + 私有 ctx；精确 fingerprint")
    }
}

impl<P: NetworkProvider> NetworkService<P> {
    pub fn dispatch(&self, _method: u32, _call: Call<'_>) -> i32 {
        todo!("校验 flat frame / 编码 / 容量，调用同一 provider 方法，写 LE 回复；不保留 Call")
    }
}
