//! 端口归属是实例语义，不是 Core 资源；Engine 内 reserve → bind → commit / abort。

use crate::Result;
use crate::address::{AddressFamily, BindAddress, InterfaceId};
use crate::socket::{SocketId, TransportProtocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortReservation {
    pub id: u64,
    pub local: BindAddress,
}

pub struct PortBinding {
    pub reservation: PortReservation,
    pub owner: SocketId,
    pub protocol: TransportProtocol,
    pub family: AddressFamily,
    pub interface: Option<InterfaceId>,
    pub committed: bool,
}

pub struct PortRegistry<'a> {
    pub entries: &'a mut [Option<PortBinding>],
    pub next_reservation_id: u64,
}

impl PortRegistry<'_> {
    pub fn reserve(
        &mut self,
        _owner: SocketId,
        _protocol: TransportProtocol,
        _family: AddressFamily,
        _local: BindAddress,
        _interface: Option<InterfaceId>,
    ) -> Result<PortReservation> {
        todo!("检查 wildcard / 具体地址重叠；TCP / UDP 分开；port=0 选临时端口，先预留")
    }

    pub fn commit(&mut self, _reservation: PortReservation, _owner: SocketId) -> Result<()> {
        todo!("原生 bind / backend 操作成功后验证精确 reservation 与 owner，再提交")
    }

    /// 未提交时即 abort；已提交时退休。accepted 连接与 listener 的保留关系仍待手写。
    pub fn release(&mut self, _reservation: PortReservation, _owner: SocketId) -> Result<()> {
        todo!("只释放精确 owner；不能因旧 backend 清理误释放后来复用的端口")
    }
}
