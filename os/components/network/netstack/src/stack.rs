//! 网络服务实例：原生 socket / 端口 / 路由语义与每接口协议后端分开。
//! 服务方法与 worker 经 service::Engine 串行访问；只有 worker 推进协议。

use smoltcp::socket::tcp::Socket as ProtocolTcpSocket;
use smoltcp::storage::{PacketBuffer, PacketMetadata};
use smoltcp::time::Instant;

use crate::Result;
use crate::address::AddressFamily;
use crate::backend::DeviceStack;
use crate::port::{PortBinding, PortRegistry};
use crate::routing::{RouteEntry, RoutingTable};
use crate::socket::{
    Attempt, SocketContext, SocketEvents, SocketId, SocketInfo, SocketObserver, SocketRecord,
    SocketSubscription, SubscriptionId,
};
use crate::tcp::{TcpConnection, TcpListener, TcpListenerStorage};
use crate::udp::{DatagramDelivery, DatagramInfo, UdpEndpoint, UdpStorage};

pub struct StackConfig<'a> {
    pub routes: &'a [RouteEntry],
}

/// runtime 预备的私有存储池；创建 / accept 在组件内取得，失败归还，不进入服务参数。
pub struct SocketPool<'a> {
    pub tcp: &'a mut [Option<ProtocolTcpSocket<'a>>],
    pub listeners: &'a mut [Option<TcpListenerStorage<'a>>],
    pub udp: &'a mut [Option<UdpStorage<'a>>],
}

/// 所有 backing 由实例 runtime 提供；每接口另有 DeviceStackStorage。
pub struct StackStorage<'a> {
    pub pool: SocketPool<'a>,
    pub records: &'a mut [Option<SocketRecord<'a>>],
    pub ports: &'a mut [Option<PortBinding>],
    pub routes: &'a mut [Option<RouteEntry>],
    pub observers: &'a mut [Option<SocketObserver>],
    pub local_metadata: &'a mut [PacketMetadata<DatagramInfo>],
    pub local_payload: &'a mut [u8],
}

pub struct Stack<'a> {
    pub pool: SocketPool<'a>,
    pub devices: &'a mut [DeviceStack<'a>],
    pub records: &'a mut [Option<SocketRecord<'a>>],
    pub ports: PortRegistry<'a>,
    pub routes: RoutingTable<'a>,
    pub observers: &'a mut [Option<SocketObserver>],
    pub local_datagrams: PacketBuffer<'a, DatagramInfo>,
    pub next_socket_id: u64,
    pub next_subscription_id: u64,
}

pub struct PollProgress {
    pub events_changed: bool,
    /// RX / socket dispatch 达到本轮 budget 时先 yield，不能直接睡眠。
    pub budget_exhausted: bool,
}

impl<'a> Stack<'a> {
    pub fn new(
        _devices: &'a mut [DeviceStack<'a>],
        _config: &StackConfig<'_>,
        _storage: StackStorage<'a>,
    ) -> Result<Self> {
        todo!("校验接口身份 / 容量，构造实例表；将路由同步到各 smoltcp Interface")
    }

    pub fn create_tcp_connection(&mut self, _family: AddressFamily) -> Result<SocketId> {
        todo!("预检表槽 / identity，从实例 pool 取私有 TCP socket 并登记 Idle；失败归还存储")
    }

    /// 保留原 identity 和端口 reservation；不先销毁 connection 再重新抢端口。
    pub fn listen_tcp(&mut self, _socket: SocketId, _pending_limit: u32) -> Result<()> {
        todo!("验证 Idle，预留 listener pool / 多接口容量，必要时临时 bind；原地转换，失败完整回滚")
    }

    pub fn create_udp_endpoint(&mut self, _family: AddressFamily) -> Result<SocketId> {
        todo!("预检表槽 / identity，从实例 pool 取 UDP backing 并构造 inbox；首次发送可自动 bind")
    }

    /// 工厂协调新对象登记；具体接管 / 补槽由 TcpListener::try_accept 实现。
    pub fn accept_tcp_connection(&mut self, _listener: SocketId) -> Result<Attempt<SocketId>> {
        todo!("验证 listener，预留 ID / 表槽 / pool 补槽；共享端口保留、接管并登记，失败不消费连接")
    }

    /// 分开借用具体对象与共享资源；closure 不得 park，借用不能逃出此调用。
    pub fn with_tcp_connection<R>(
        &mut self,
        _socket: SocketId,
        _operation: impl FnOnce(&mut TcpConnection<'a>, &mut SocketContext<'_, 'a>) -> Result<R>,
    ) -> Result<R> {
        todo!("校验 identity / 未退役 / 类型；拆分 records 与 context 后执行 connection 操作")
    }

    pub fn with_tcp_listener<R>(
        &mut self,
        _socket: SocketId,
        _operation: impl FnOnce(&mut TcpListener<'a>, &mut SocketContext<'_, 'a>) -> Result<R>,
    ) -> Result<R> {
        todo!("校验 identity / 未退役 / 类型后借用 listener；不泛化出字节流收发")
    }

    pub fn with_udp_endpoint<R>(
        &mut self,
        _socket: SocketId,
        _operation: impl FnOnce(&mut UdpEndpoint<'a>, &mut SocketContext<'_, 'a>) -> Result<R>,
    ) -> Result<R> {
        todo!("校验 identity / 未退役 / 类型后借用 UDP endpoint 与 context")
    }

    pub fn socket_info(&self, _socket: SocketId) -> Result<SocketInfo> {
        todo!("验证活端点，静态分发 InetSocket::info；不暴露 smoltcp handle")
    }

    pub fn socket_events(&self, _socket: SocketId) -> Result<SocketEvents> {
        todo!("验证活端点，静态分发 InetSocket::events；不内联 poll 或消费事件")
    }

    pub fn subscribe(
        &mut self,
        _socket: SocketId,
        _observer_task: u32,
    ) -> Result<SocketSubscription> {
        todo!("校验 task / 槽位 / identity，登记后返回同轮快照；发布变化后跨组件通知，防漏唤醒")
    }

    pub fn unsubscribe(&mut self, _subscription: SubscriptionId) -> Result<()> {
        todo!("退休精确 subscription，处理在途通知；不复用旧 identity 或保留死亡 task")
    }

    pub fn release_socket(&mut self, _socket: SocketId) -> Result<()> {
        todo!(
            "先安排最终通知再退休观察者 / ID；保留清理所需 buffers / 端口，listener 不伤及 accepted"
        )
    }

    pub fn dispatch_udp(&mut self, _packet_budget: usize) -> Result<bool> {
        todo!("有界提取设备 RX 与 local_datagrams，统一 demux；返回预算耗尽，处理队列满 / 丢包")
    }

    pub fn deliver_udp(
        &mut self,
        _info: DatagramInfo,
        _payload: &[u8],
    ) -> Result<DatagramDelivery> {
        todo!("匹配地址族 / 实际目的端点 / 接口 / receive_peer；容量预检后入队并推进 revision")
    }

    pub fn poll(&mut self, _now: Instant, _rx_budget: usize) -> Result<PollProgress> {
        todo!("公平有界推进设备 / connection / listener，分发 UDP，先发布 revision / events 再通知")
    }

    /// 汇总各设备的协议事件；None 表示目前不登记协议 timer。
    /// 不是每次等待的兜底超时，也不保证任务在该时刻实际获得 CPU。
    pub fn protocol_deadline(&mut self, _now: Instant) -> Option<Instant> {
        todo!("取各 DeviceStack::protocol_deadline 的最早时间，纳入待关闭连接等协议工作")
    }
}
