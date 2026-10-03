//! netstack 的服务入口；消费者只经 SDK NetworkBinding 调用。
//! 这里只声明同步边界和分发入口，操作体仍由人类手写。

use core::cell::UnsafeCell;
use core::sync::atomic::AtomicBool;

use kcomp_sdk::network::*;

use crate::Result;
use crate::stack::Stack;

/// 实例内唯一同步点；服务与 worker 都经它访问 Stack。
/// 当前没有锁实现 / Sync 承诺。实现时使用非阻塞尝试，竞争返回 Busy；
/// 不在 guard 内 park、yield、调用 driver / Core 通知或其他组件。
/// 成功 guard 必须覆盖所有可变借用，释放后再进行外部调用。
pub struct Engine<'a> {
    _busy: AtomicBool,
    _stack: UnsafeCell<Stack<'a>>,
}

impl<'a> Engine<'a> {
    pub fn new(_stack: Stack<'a>) -> Self {
        todo!("构造实例同步点；在发布服务前完成所有 backing 初始化")
    }

    pub fn try_with<R>(&self, _operation: impl FnOnce(&mut Stack<'a>) -> Result<R>) -> Result<R> {
        todo!("尝试 Acquire 获得独占借用，失败 Busy；执行有界操作并 Release，禁止借用逃逸")
    }
}

pub struct NetworkInstance<'a> {
    pub engine: Engine<'a>,
    pub worker_task: u32,
}

/// 每个方法通过 Engine 验证 / 操作内部对象；在适配边界集中映射内部错误。
/// Busy 绝不映射 Pending，调用者 buffer 只借用到返回。事件变化先提交，再锁外通知。
impl NetworkProvider for NetworkInstance<'_> {
    fn tcp_open(&self, _family: AddressFamily) -> NetworkResult<SocketId> {
        todo!("Engine 内从 Stack 私有池创建 connection；无数据缓冲跨边界")
    }

    fn udp_open(&self, _family: AddressFamily) -> NetworkResult<SocketId> {
        todo!("Engine 内从私有池创建 UDP endpoint")
    }

    fn socket_bind(&self, _socket: SocketId, _local: BindAddress) -> NetworkResult<()> {
        todo!("Engine 内校验 ID / 类型，分发 bind_local，立即预留端口")
    }

    fn tcp_connect(&self, _socket: SocketId, _peer: SocketAddress) -> NetworkResult<()> {
        todo!("Engine 内 with_tcp_connection 发起握手；提交状态，解锁后唤醒 worker")
    }

    fn tcp_listen(&self, _socket: SocketId, _pending_limit: u32) -> NetworkResult<()> {
        todo!("Engine 内 Stack::listen_tcp 原地转换；保留 ID / 端口，失败完整回滚")
    }

    fn tcp_accept(&self, _listener: SocketId) -> NetworkResult<Attempt<SocketId>> {
        todo!("Engine 内 Stack::accept_tcp_connection；池 / 表槽不足报错，无连接才 Pending")
    }

    fn tcp_status(&self, _socket: SocketId) -> NetworkResult<ConnectionStatus> {
        todo!("Engine 内查询 connection；listener 返回 Listening / Closed；纯快照不 poll")
    }

    fn tcp_send(&self, _socket: SocketId, _data: &[u8]) -> NetworkResult<Attempt<usize>> {
        todo!("Engine 内借用 connection 和 ctx，复制有界 payload 到 TX；解锁后唤醒 worker")
    }

    fn tcp_receive(
        &self,
        _socket: SocketId,
        _buffer: &mut [u8],
    ) -> NetworkResult<Attempt<StreamReceive>> {
        todo!("Engine 内有界复制到调用方 buffer，释放后不保留指针；RX 变化后唤醒 worker")
    }

    fn tcp_finish_send(&self, _socket: SocketId) -> NetworkResult<()> {
        todo!("Engine 内 connection.finish_send；解锁后 worker 推进 FIN")
    }

    fn tcp_abort(&self, _socket: SocketId) -> NetworkResult<()> {
        todo!("Engine 内 connection.abort；保留查询身份，解锁后推进 RST / 清理")
    }

    fn udp_send_to(
        &self,
        _socket: SocketId,
        _peer: SocketAddress,
        _data: &[u8],
    ) -> NetworkResult<Attempt<()>> {
        todo!("Engine 内整包提交 / 必要时自动绑定，Pending / 错误回滚；解锁后唤醒 worker")
    }

    fn udp_receive(
        &self,
        _socket: SocketId,
        _buffer: &mut [u8],
        _mode: DatagramReadMode,
    ) -> NetworkResult<Attempt<ReceivedDatagram>> {
        todo!("Engine 内验证已绑定，再从 inbox 按模式复制；不消费时保留包，解锁后通知变化")
    }

    fn udp_set_receive_peer(
        &self,
        _socket: SocketId,
        _peer: Option<SocketAddress>,
    ) -> NetworkResult<()> {
        todo!("Engine 内验证 UDP / 地址族，改变后续投递过滤")
    }

    fn socket_info(&self, _socket: SocketId) -> NetworkResult<SocketInfo> {
        todo!("Engine 内 Stack::socket_info，持有同步点读取快照")
    }

    fn socket_events(&self, _socket: SocketId) -> NetworkResult<SocketEvents> {
        todo!("Engine 内 Stack::socket_events，纯查询")
    }

    fn socket_subscribe(&self, _socket: SocketId, _task: u32) -> NetworkResult<SocketSubscription> {
        todo!("Engine 内登记观察者并读取同一提交的快照；外部 task 通知机制尚待实现")
    }

    fn socket_unsubscribe(&self, _subscription: SubscriptionId) -> NetworkResult<()> {
        todo!("Engine 内退休精确登记；不复用旧 identity，不保证清掉已有 task permit")
    }

    fn socket_release(&self, _socket: SocketId) -> NetworkResult<()> {
        todo!("Engine 内逻辑退役并收集最终通知；解锁后通知，不等待 TCP 关闭")
    }
}
