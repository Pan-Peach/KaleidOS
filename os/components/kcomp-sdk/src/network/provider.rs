//! Local business interface; Endpoint IPC handlers remain unimplemented.

use super::*;

/// 业务 facade：只接收值、对象 ID 和本次调用借用的 payload。
/// 方法不 park、不轮询网卡；未来 IPC Server 在本镜像内调用这些普通方法。
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
