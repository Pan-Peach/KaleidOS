//! consumer 代理；保存绑定、ID 和不可变地址族，不借用 provider 内存。

use crate::endpoint::Endpoint;
use crate::errno::Errno;

use super::*;

/// TODO: 替换为 Core bind 一次选定的 Direct / Gate 后端，不能跨执行域转移。
pub struct NetworkBinding {
    _endpoint: Endpoint<Network>,
}

impl Endpoint<Network> {
    pub fn bind(&self) -> NetworkResult<NetworkBinding> {
        // TODO: exact contract / ABI / liveness 校验；目前无适配器，明确拒绝。
        Err(NetworkError::Method(Errno::ENOTSUP.code()))
    }
}

impl NetworkBinding {
    pub fn tcp_socket(&self, _family: AddressFamily) -> NetworkResult<TcpSocket<'_>> {
        todo!("调用 tcp_open，由服务分配对象和缓冲区；返回未绑定的 TCP 代理")
    }

    pub fn udp_socket(&self, _family: AddressFamily) -> NetworkResult<UdpSocket<'_>> {
        todo!("调用 udp_open；返回未绑定的 UDP 代理，不向调用方索取 smoltcp storage")
    }
}

/// 共有的本地代理能力；此 trait object 绝不跨组件 ABI。
pub trait InetSocket {
    fn info(&self) -> NetworkResult<SocketInfo>;
    fn events(&self) -> NetworkResult<SocketEvents>;
    fn subscribe(&self, task: u32) -> NetworkResult<Subscription<'_>>;
    /// 显式释放；成功后本代理失效。无隐式 Drop I/O，需在错误路径同样清理。
    fn close(&mut self) -> NetworkResult<()>;
}

/// 同一 TCP identity 可由 Idle 转 Connecting 或 Listening，先 bind 再 listen 保留端口。
/// 无 Clone / Copy；personality 的 dup 可以共享一个代理并在最后引用处 close。
pub struct TcpSocket<'net> {
    _network: &'net NetworkBinding,
    _id: SocketId,
    /// wildcard bind 编码所需；accept 继承监听对象的地址族。
    _family: AddressFamily,
}

impl<'net> TcpSocket<'net> {
    pub fn bind_local(&self, _local: BindAddress) -> NetworkResult<()> {
        todo!("socket_bind：立刻预留本地端口；失败不改变原对象")
    }

    /// 未绑定时由服务选择临时端口；成功只表示握手发起，完成由 status 观察。
    pub fn start_connect(&self, _peer: SocketAddress) -> NetworkResult<()> {
        todo!("tcp_connect；不等待握手完成，不保留调用方请求")
    }

    pub fn listen(&self, _pending_limit: u32) -> NetworkResult<()> {
        todo!("tcp_listen：同一 identity 转为 listener；失败保持原绑定 / Idle 状态")
    }

    pub fn try_accept(&self) -> NetworkResult<Attempt<TcpSocket<'net>>> {
        todo!("tcp_accept；成功创建独立代理，补监听槽 / 缓冲分配由服务处理")
    }

    pub fn status(&self) -> NetworkResult<ConnectionStatus> {
        todo!("tcp_status；校验 phase / flags / 持久错误，不实施读后清除")
    }

    pub fn try_send(&self, _data: &[u8]) -> NetworkResult<Attempt<usize>> {
        todo!("tcp_send；保留部分写，EAGAIN 映射 Pending，EBUSY 独立返回 Busy")
    }

    pub fn try_receive(&self, _buffer: &mut [u8]) -> NetworkResult<Attempt<StreamReceive>> {
        todo!("tcp_receive；校验实际长度，区分 Bytes(0) / End / Pending")
    }

    pub fn finish_send(&self) -> NetworkResult<()> {
        todo!("tcp_finish_send：FIN；接收方向继续，监听对象拒绝此操作")
    }

    pub fn abort(&self) -> NetworkResult<()> {
        todo!("tcp_abort：中止连接，状态仍可查询；不是释放代理")
    }
}

impl InetSocket for TcpSocket<'_> {
    fn info(&self) -> NetworkResult<SocketInfo> {
        todo!("socket_info；ID / 当前绑定 / 接口，不返回 backend handle")
    }

    fn events(&self) -> NetworkResult<SocketEvents> {
        todo!("socket_events；查询已发布状态")
    }

    fn subscribe(&self, _task: u32) -> NetworkResult<Subscription<'_>> {
        todo!("socket_subscribe；返回登记 ID 和同一同步点的快照")
    }

    fn close(&mut self) -> NetworkResult<()> {
        todo!("socket_release 成功后置 ID 为无效；失败保持代理，不自动重试")
    }
}

pub struct UdpSocket<'net> {
    _network: &'net NetworkBinding,
    _id: SocketId,
    _family: AddressFamily,
}

impl UdpSocket<'_> {
    pub fn bind_local(&self, _local: BindAddress) -> NetworkResult<()> {
        todo!("socket_bind：覆盖适用接口，实际分配的端口可从 info 查询")
    }

    pub fn set_receive_peer(&self, _peer: Option<SocketAddress>) -> NetworkResult<()> {
        todo!("udp_set_receive_peer：只改变后续包过滤，保留已有 inbox")
    }

    pub fn try_send_to(&self, _peer: SocketAddress, _data: &[u8]) -> NetworkResult<Attempt<()>> {
        todo!("udp_send_to：未绑定时自动绑定临时端口；整包入队或 Pending")
    }

    pub fn try_receive(
        &self,
        _buffer: &mut [u8],
        _mode: DatagramReadMode,
    ) -> NetworkResult<Attempt<ReceivedDatagram>> {
        todo!("udp_receive；校验 copied / original_len / consumed；零长度包也为 Ready")
    }
}

impl InetSocket for UdpSocket<'_> {
    fn info(&self) -> NetworkResult<SocketInfo> {
        todo!("socket_info")
    }

    fn events(&self) -> NetworkResult<SocketEvents> {
        todo!("socket_events")
    }

    fn subscribe(&self, _task: u32) -> NetworkResult<Subscription<'_>> {
        todo!("socket_subscribe；登记后还需重试操作")
    }

    fn close(&mut self) -> NetworkResult<()> {
        todo!("socket_release 成功后置 ID 为无效；失败保持代理")
    }
}

/// 必须显式取消。借用 socket，防止安全 SDK 在本订阅尚持有时 close 该代理。
pub struct Subscription<'socket> {
    _network: &'socket NetworkBinding,
    _id: SubscriptionId,
    pub current: SocketEvents,
}

impl Subscription<'_> {
    pub fn cancel(&mut self) -> NetworkResult<()> {
        todo!("socket_unsubscribe 成功后失效；取消前在途通知可造成一次无害的额外唤醒")
    }
}
