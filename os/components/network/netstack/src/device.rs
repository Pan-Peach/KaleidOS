//! NetDevice binding 草案与 smoltcp::phy 的真实 trait 接缝。
//! NetDevice 契约 / SDK binding 尚未定稿，不声明假的 kcore_net_*。
//! 网卡驱动拥有 MMIO / IRQ / DMA；本 adapter 只处理完整 Ethernet frame。

use smoltcp::phy::{Device, DeviceCapabilities, RxToken, TxToken};
use smoltcp::time::Instant;

use crate::Result;
use crate::address::InterfaceId;
use crate::socket::Attempt;

pub struct DeviceInfo {
    pub mac: [u8; 6],
    /// smoltcp Ethernet max_transmission_unit：完整帧长度，不含 FCS。
    pub frame_mtu: usize,
}

/// 组件内草案：endpoint 必须来自组合配置，未来经 SDK exact ABI 校验 / bind。
/// 不持 Core MMIO / DMA 指针，不按名字扫描或加载默认网卡。
pub struct NetDeviceBinding {
    pub endpoint: u64,
    pub info: DeviceInfo,
}

impl NetDeviceBinding {
    pub fn bind(_endpoint: u64) -> Result<Self> {
        todo!("定稿 NetDevice C ABI；经 SDK 校验 / bind endpoint 并查询 MAC / MTU")
    }

    pub fn receive(&mut self, _buffer: &mut [u8]) -> Result<Option<usize>> {
        todo!("非阻塞取一个完整帧；无包返回 None，校验长度 / provider 状态")
    }

    pub fn transmit(&mut self, _frame: &[u8]) -> Result<Attempt<()>> {
        todo!("发送完整帧；队列满 Pending，成功 Ready(())，失败 Result；不伪造入队成功")
    }

    pub fn subscribe(&mut self, _worker_task: u32) -> Result<()> {
        todo!("登记 worker TaskId；driver 在发布 RX / TX 完成后跨组件 unpark")
    }

    pub fn unsubscribe(&mut self, _worker_task: u32) -> Result<()> {
        todo!("撤销通知关系并处理在途通知；不保留退休实例的等待者")
    }
}

/// RX / TX staging storage 属于本实例；先采用复制，不设计跨域裸指针共享。
pub struct SmoltcpDevice<'a> {
    pub info: DeviceInfo,
    pub rx_buffer: &'a mut [u8],
    pub rx_len: Option<usize>,
    pub tx_buffer: &'a mut [u8],
    pub tx_len: Option<usize>,
}

/// worker 独占 driver binding 和锁外帧副本，避免持有 Engine guard 调用别的组件。
pub struct DevicePort<'a> {
    pub interface: InterfaceId,
    pub provider: NetDeviceBinding,
    pub rx_frame: &'a mut [u8],
    pub rx_pending: Option<usize>,
    pub tx_frame: &'a mut [u8],
    pub tx_pending: Option<usize>,
}

impl SmoltcpDevice<'_> {
    pub fn stage_receive(&mut self, _frame: &[u8]) -> Result<Attempt<()>> {
        todo!("在同步点内校验完整帧并复制；槽满 Pending，worker 保留锁外 RX 副本")
    }

    pub fn take_transmit(&mut self, _buffer: &mut [u8]) -> Result<Attempt<usize>> {
        todo!("有界复制已提交 TX 帧；buffer 足够才移出；worker 持有到 driver 接受或明确失败")
    }
}

pub struct SmoltcpRxToken<'a> {
    pub frame: &'a [u8],
}

pub struct SmoltcpTxToken<'a> {
    pub buffer: &'a mut [u8],
    pub committed_len: &'a mut Option<usize>,
}

impl Device for SmoltcpDevice<'_> {
    type RxToken<'a>
        = SmoltcpRxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = SmoltcpTxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        todo!("仅访问本地 RX staging；先预留回复 TX 槽，无包 / 无槽返回 None；不外调 driver")
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        todo!("检查 / 预留 TX 容量后才交付 token；队列满返回 None")
    }

    fn capabilities(&self) -> DeviceCapabilities {
        todo!("Ethernet medium、完整帧 MTU；未提供的 checksum offload 不宣称支持")
    }
}

impl RxToken for SmoltcpRxToken<'_> {
    fn consume<R, F>(self, _f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        todo!("把已校验的完整帧交给 smoltcp closure，消费后释放 RX 借用")
    }
}

impl TxToken for SmoltcpTxToken<'_> {
    fn consume<R, F>(self, _len: usize, _f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        todo!("校验 len、填本地预留帧并记录长度；driver 提交留到解锁后，背压不丢帧")
    }
}
