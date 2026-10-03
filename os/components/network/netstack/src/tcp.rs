//! provider 内部 TCP connection / listener；其他组件调用 SDK network::TcpSocket。
//! service.rs 将服务方法分发到这里，SocketContext 不出本镜像。

use smoltcp::socket::tcp::Socket as ProtocolTcpSocket;

use crate::Result;
use crate::address::{BindAddress, SocketAddress};
use crate::backend::BackendRef;
use crate::socket::{
    Attempt, InetSocket, SocketCommon, SocketContext, SocketEvents, SocketId, SocketInfo,
};

pub use kcomp_sdk::network::{ConnectionPhase, ConnectionStatus, StreamReceive};

pub struct TcpBuffers<'a> {
    pub rx: &'a mut [u8],
    pub tx: &'a mut [u8],
}

impl<'a> TcpBuffers<'a> {
    /// runtime 准备私有 smoltcp 对象；成功才取 buffers，随后可在 attach 失败时保留原对象。
    pub fn prepare(_buffers: &mut Option<Self>) -> Result<ProtocolTcpSocket<'a>> {
        todo!("先校验 RX / TX 容量，再构造 SocketBuffer / tcp::Socket；不选择网卡或登记 identity")
    }
}

// 固定表内联存储待挂载 socket，保留失败回滚所需 buffers；不引入 Box / alloc。
#[allow(clippy::large_enum_variant)]
pub enum ConnectionState<'a> {
    Idle(ProtocolTcpSocket<'a>),
    Connecting {
        backend: BackendRef,
        peer: SocketAddress,
    },
    Established {
        backend: BackendRef,
        peer: SocketAddress,
    },
    Closing {
        backend: BackendRef,
        peer: SocketAddress,
    },
    Closed,
}

pub struct TcpConnection<'a> {
    pub common: SocketCommon,
    pub state: ConnectionState<'a>,
    pub send_finished: bool,
    pub receive_ended: bool,
    pub failure: Option<i32>,
}

impl InetSocket for TcpConnection<'_> {
    fn info(&self) -> SocketInfo {
        todo!("返回 connection 的已发布端点信息")
    }

    fn events(&self) -> SocketEvents {
        todo!("读取已发布事件；FIN / reset / 连接完成通过 revision 与 status 观察")
    }
}

impl<'a> TcpConnection<'a> {
    pub fn bind_local(
        &mut self,
        _ctx: &mut SocketContext<'_, 'a>,
        _local: BindAddress,
    ) -> Result<()> {
        todo!("验证 Idle / 地址族，预留与提交端口；port=0 显式请求分配，失败保持原状态")
    }

    /// 未绑定时自动选临时端口；成功仅表示握手已发起，完成 / 失败由 status 观察。
    pub fn start_connect(
        &mut self,
        _ctx: &mut SocketContext<'_, 'a>,
        _peer: SocketAddress,
    ) -> Result<()> {
        todo!("按约束路由 / 必要时临时绑定，延迟 attach 后 connect；失败回滚 Idle / 自动绑定")
    }

    pub fn status(&self) -> ConnectionStatus {
        todo!("读取连接状态 / 方向结束 / 持久失败；不 poll 或消费 personality 的错误")
    }

    /// Ready(n) 仅表示部分或全部数据进入发送缓冲，不代表 peer 收到或 ACK。
    pub fn try_send(
        &mut self,
        _ctx: &mut SocketContext<'_, 'a>,
        _data: &[u8],
    ) -> Result<Attempt<usize>> {
        todo!("验证连接 / 发送方向，经 backend send_slice 入队；背压 Pending，失败 Result")
    }

    pub fn try_receive(
        &mut self,
        _ctx: &mut SocketContext<'_, 'a>,
        _buffer: &mut [u8],
    ) -> Result<Attempt<StreamReceive>> {
        todo!("有数据返回 Bytes；FIN 且已排空返回 End；暂无数据 Pending；reset 返回错误")
    }

    /// 排空已排队发送数据后发 FIN；接收方向仍可继续，不定义 SHUT_RD 行为。
    pub fn finish_send(&mut self, _ctx: &mut SocketContext<'_, 'a>) -> Result<()> {
        todo!("关闭新发送并映射 smoltcp close；继续 poll 完成 FIN / 协议关闭")
    }

    pub fn abort(&mut self, _ctx: &mut SocketContext<'_, 'a>) -> Result<()> {
        todo!("中止连接并驱动必要的 RST / 清理；不是关闭某个 fd 或取消某次应用请求")
    }
}

/// 明确的 pending connection 容量；不直接采用某个 OS 的 backlog 数值解释。
pub struct TcpListenerStorage<'a> {
    pub slots: &'a mut [Option<BackendRef>],
    /// runtime 用 TcpBuffers::prepare 构造；失败回滚时归还完整对象，无需提取私有 buffers。
    pub sockets: &'a mut [Option<ProtocolTcpSocket<'a>>],
}

pub struct TcpListener<'a> {
    pub common: SocketCommon,
    pub storage: TcpListenerStorage<'a>,
    pub pending_limit: usize,
    pub stopped: bool,
}

pub struct AcceptedConnection {
    pub backend: BackendRef,
    pub local: SocketAddress,
    pub peer: SocketAddress,
}

impl InetSocket for TcpListener<'_> {
    fn info(&self) -> SocketInfo {
        todo!("返回 listener 的已发布端点信息")
    }

    fn events(&self) -> SocketEvents {
        todo!("incoming_connection 表示存在可接管连接，不把它解释为字节流可读")
    }
}

impl<'a> TcpListener<'a> {
    /// Stack 先预留新 connection 的 identity / 表槽；Pending 或失败不消费 replacement。
    pub fn try_accept(
        &mut self,
        _ctx: &mut SocketContext<'_, 'a>,
        _new_owner: SocketId,
        _replacement: &mut Option<ProtocolTcpSocket<'a>>,
    ) -> Result<Attempt<AcceptedConnection>> {
        todo!("取已建立连接，补槽并转交 backend owner；返回实际本地 / peer 端点，协调回滚")
    }

    /// 停止监听并取消未接管的连接；已转交的 connection 不受影响。
    pub fn stop(&mut self, _ctx: &mut SocketContext<'_, 'a>) -> Result<()> {
        todo!("停止新握手，清理 listener 多 backend / 待接管连接，发布状态变化")
    }
}
