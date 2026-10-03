//! 网络 worker 骨架：独占协议推进和 driver binding；与服务共享受控的实例状态。
//! Core 跨组件 unpark / 组件 timer ABI 尚未落地，本文件不伪造已存在的机制。
//! 普通 park 没有兜底 deadline；可在本组件封装 timed wait，不新增 Core park_until。

use smoltcp::time::Instant;

use crate::Result;
use crate::clock::Clock;
use crate::device::DevicePort;
use crate::service::NetworkInstance;

/// 组件内占位；真实登记 identity / 取消契约须等 Core timer ABI 定稿。
pub struct TimerRegistration(pub u64);

pub struct NetworkWorker<'worker, 'a> {
    pub instance: &'worker NetworkInstance<'a>,
    pub devices: &'worker mut [DevicePort<'a>],
    pub clock: Clock,
    pub task: u32,
    pub protocol_timer: Option<TimerRegistration>,
}

impl NetworkWorker<'_, '_> {
    pub fn run(&mut self, _rx_budget: usize) -> Result<()> {
        todo!(
            "锁外 driver 收发；Engine 内交换帧 / 有界 poll / 发布事件，解锁后通知；Busy 先 yield，不 park"
        )
    }

    pub fn notify(&self) -> Result<()> {
        todo!("服务已提交状态后唤醒 worker；不在 Engine guard 内外调，跨 owner 许可仍待 Core 改造")
    }

    pub fn arm_protocol_timer(&mut self, _deadline: Instant) -> Result<()> {
        todo!("显式登记 / 更新一次定时唤醒，检查 tick 换算和旧登记失效；不是漏通知兜底")
    }

    pub fn cancel_protocol_timer(&mut self) -> Result<()> {
        todo!("取消登记，定义取消与到期竞态；过期事件不能完成下一次业务等待")
    }

    pub fn wait(&mut self) -> Result<()> {
        todo!("条件检查循环 + 普通 kcore_task_park；permit 保留提前通知，醒后重新判断")
    }

    pub fn stop(&mut self) -> Result<()> {
        todo!("停止新操作，退订 driver 通知 / 取消 timer，协调 worker 与 socket 逻辑退役")
    }
}
