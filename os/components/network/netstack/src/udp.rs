//! UDP endpoint 原语；目的端点、peer 过滤与短 buffer 处理均显式指定。

use smoltcp::socket::udp::PacketMetadata as BackendPacketMetadata;
use smoltcp::socket::udp::Socket as ProtocolUdpSocket;
use smoltcp::storage::{PacketBuffer, PacketMetadata};

use crate::Result;
use crate::address::{BindAddress, SocketAddress};
use crate::backend::BackendRef;
use crate::socket::{Attempt, InetSocket, SocketCommon, SocketContext, SocketEvents, SocketInfo};

pub use kcomp_sdk::network::{DatagramInfo, DatagramReadMode, ReceivedDatagram};

pub struct UdpBuffers<'a> {
    pub rx_metadata: &'a mut [BackendPacketMetadata],
    pub rx_payload: &'a mut [u8],
    pub tx_metadata: &'a mut [BackendPacketMetadata],
    pub tx_payload: &'a mut [u8],
}

impl<'a> UdpBuffers<'a> {
    pub fn prepare(_buffers: &mut Option<Self>) -> Result<ProtocolUdpSocket<'a>> {
        todo!("先校验 metadata / payload 容量，成功才取 buffers 并构造私有 UDP socket")
    }
}

pub struct UdpStorage<'a> {
    pub backends: &'a mut [Option<BackendRef>],
    pub sockets: &'a mut [Option<ProtocolUdpSocket<'a>>],
    pub rx_metadata: &'a mut [PacketMetadata<DatagramInfo>],
    pub rx_payload: &'a mut [u8],
}

pub struct UdpEndpoint<'a> {
    pub common: SocketCommon,
    pub receive_peer: Option<SocketAddress>,
    /// wildcard RX 覆盖适用接口，TX 按路由选择 backend。
    pub backends: &'a mut [Option<BackendRef>],
    pub sockets: &'a mut [Option<ProtocolUdpSocket<'a>>],
    pub inbox: PacketBuffer<'a, DatagramInfo>,
    pub stopped: bool,
}

pub enum DatagramDelivery {
    Delivered,
    NoRecipient,
    QueueFull,
}

impl InetSocket for UdpEndpoint<'_> {
    fn info(&self) -> SocketInfo {
        todo!("返回 UDP endpoint 的已发布端点信息")
    }

    fn events(&self) -> SocketEvents {
        todo!("依据原生 inbox / 发送容量发布事件，不以单个 backend 的 can_recv 代替")
    }
}

impl<'a> UdpEndpoint<'a> {
    pub fn bind_local(
        &mut self,
        _ctx: &mut SocketContext<'_, 'a>,
        _local: BindAddress,
    ) -> Result<()> {
        todo!("预留端口，覆盖适用接口的 RX backend，成功再 commit；失败整批回滚")
    }

    /// 仅控制接收匹配；默认发送目的地和 UDP connect 的应用规则由 personality 保存。
    /// 过滤只对后续投递生效，已有 inbox 保留；应用需要清队列时显式接收 / 丢弃。
    pub fn set_receive_peer(&mut self, _peer: Option<SocketAddress>) -> Result<()> {
        todo!("校验地址族，设置或解除后续投递的 peer 过滤，保留 inbox 并发布变化，不发握手")
    }

    /// 未绑定时自动选端口；Ready 表示整包入队，Pending 回滚首次绑定且不消费数据。
    pub fn try_send_to(
        &mut self,
        _ctx: &mut SocketContext<'_, 'a>,
        _peer: SocketAddress,
        _data: &[u8],
    ) -> Result<Attempt<()>> {
        todo!("预检整包容量 / 路由，必要时自动绑定；本地 staging 或 backend 入队，失败回滚")
    }

    /// Ready 可包含零长度数据报；无包才是 Pending，短 buffer 不隐式选择消费规则。
    pub fn try_receive(
        &mut self,
        _buffer: &mut [u8],
        _mode: DatagramReadMode,
    ) -> Result<Attempt<ReceivedDatagram>> {
        todo!("只读原生 inbox；按模式复制 / 消费，返回实际复制量、原始长度、消费标记")
    }

    pub fn stop(&mut self, _ctx: &mut SocketContext<'_, 'a>) -> Result<()> {
        todo!("停止新收发，协调多 backend 与 inbox 清理，发布状态；不定义应用描述符 close")
    }
}
