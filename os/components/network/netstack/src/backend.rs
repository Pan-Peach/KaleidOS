//! 每接口的 smoltcp 串行域；原生 socket 可拥有零个、一个或多个 backend。
//! 参考 MangoCore 的每设备栈 / 间接 handle 思路；协议推进只由 worker 执行。

use smoltcp::iface::{Interface, SocketHandle, SocketSet, SocketStorage};
use smoltcp::socket::{Socket, tcp, udp};
use smoltcp::time::Instant;

use crate::Result;
use crate::address::{InterfaceAddress, InterfaceId};
use crate::device::{DeviceInfo, SmoltcpDevice};
use crate::socket::{SocketId, TransportProtocol};
use crate::stack::PollProgress;

/// 本实例内发号、检查耗尽；不能把可复用 SocketHandle 当成此 identity。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendRef {
    pub interface: InterfaceId,
    pub id: BackendId,
}

pub struct BackendBinding {
    pub id: BackendId,
    pub owner: SocketId,
    pub protocol: TransportProtocol,
    pub handle: SocketHandle,
}

pub struct DeviceStackConfig<'a> {
    pub id: InterfaceId,
    pub addresses: &'a [InterfaceAddress],
    /// 组合者 / runtime 提供 seed；不以固定常量宣称随机初始化已实现。
    pub random_seed: u64,
}

pub struct DeviceStackStorage<'a> {
    pub sockets: &'a mut [SocketStorage<'a>],
    pub bindings: &'a mut [Option<BackendBinding>],
    pub rx_frame: &'a mut [u8],
    pub tx_frame: &'a mut [u8],
}

pub struct DeviceStack<'a> {
    pub id: InterfaceId,
    pub interface: Interface,
    pub sockets: SocketSet<'a>,
    pub device: SmoltcpDevice<'a>,
    pub bindings: &'a mut [Option<BackendBinding>],
    pub next_backend_id: u64,
}

impl<'a> DeviceStack<'a> {
    pub fn new(
        _info: DeviceInfo,
        _config: &DeviceStackConfig<'_>,
        _storage: DeviceStackStorage<'a>,
        _now: Instant,
    ) -> Result<Self> {
        todo!("校验帧存储 / MAC / 地址，构造此接口的 Config、Interface、SocketSet")
    }

    /// 成功才取走 socket；失败保留原对象，让调用者恢复 buffers / 原生状态。
    pub fn attach(
        &mut self,
        _owner: SocketId,
        _socket: &mut Option<Socket<'a>>,
    ) -> Result<BackendRef> {
        todo!("预检空槽和 identity，插入 socket 与 binding 后发布；失败回滚")
    }

    /// 只在验证身份、owner、协议后访问；引用不能逃出 closure 或跨 park。
    pub fn with_tcp<R>(
        &mut self,
        _backend: BackendRef,
        _owner: SocketId,
        _operation: impl FnOnce(&mut tcp::Socket<'a>) -> R,
    ) -> Result<R> {
        todo!("重验 interface / BackendId / owner / Tcp；不让旧 token 命中复用 slot")
    }

    pub fn with_udp<R>(
        &mut self,
        _backend: BackendRef,
        _owner: SocketId,
        _operation: impl FnOnce(&mut udp::Socket<'a>) -> R,
    ) -> Result<R> {
        todo!("重验 interface / BackendId / owner / Udp 后调用 closure")
    }

    pub fn detach(&mut self, _backend: BackendRef, _owner: SocketId) -> Result<Socket<'a>> {
        todo!("验证身份并退休 binding，再移出 socket；保留 buffers，不靠重建丢弃队列")
    }

    pub fn transfer(
        &mut self,
        _backend: BackendRef,
        _previous: SocketId,
        _new_owner: SocketId,
    ) -> Result<()> {
        todo!("accept 提交时验证旧 owner 并转交 binding；新 owner 已预留，失败不改变原归属")
    }

    pub fn poll(&mut self, _now: Instant, _rx_budget: usize) -> Result<PollProgress> {
        todo!("有界 poll_ingress_single / poll_maintenance / poll_egress；不发送业务通知")
    }

    pub fn protocol_deadline(&mut self, _now: Instant) -> Option<Instant> {
        todo!("通过 Interface::poll_at 查询此接口下一次协议工作时间")
    }
}
