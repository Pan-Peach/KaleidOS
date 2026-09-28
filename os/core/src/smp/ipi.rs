//! Core 的 pending IPI 与分发。
//!
//! # 排序契约（arch 不负责）
//!
//! Core **先发布 pending work，再让 arch 响门铃**；arch 的“发布 → 硬件通知”
//! 排序由后端负责（一次 Rust release 不能替代 MMIO/固件通知屏障）。接收端在
//! 硬件回调里只**标记** work，真正处理放到定义好的安全边界（不在任意上下文里
//! 直接切换调度）。
//!
//! 门铃可合并、可重复投递；`send_ipi_mask` 可能**部分投递**后才返回错误——
//! Core 必须保留 pending work，不得把失败理解为“什么都没送到”。
//!
//! # 骨架状态
//!
//! 先只做实际需要的 `Reschedule`。**不**预先塞一个“无载荷 TLB shootdown 标志”
//! 就宣称完成：正确的 shootdown 需要受影响的地址空间/范围、确认与生存期规则。

use crate::machine::CpuId;
use crate::smp::CpuMask;

/// 一次 IPI 请求（Core 语义；arch 只传门铃，不解释这些值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpiRequest {
    /// 请求目标 CPU 在安全边界重新调度。
    Reschedule,
}

/// [`notify`] 的失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyError {
    /// 目标集合里含无效逻辑 CPU。
    InvalidTarget,
    /// 后端投递失败（可能已部分投递）。
    DeliveryFailed,
}

/// 向一组 CPU 发布并投递一次 IPI 请求。
pub(crate) fn notify(_targets: &CpuMask, _request: IpiRequest) -> Result<(), NotifyError> {
    todo!("SMP: publish pending work then ring each target's doorbell")
}

/// 本 CPU 的 IPI 硬件回调（由 `arch::Smp::register_ipi_handler` 注册）。
///
/// 只标记 pending，不在此处做调度切换。
pub(crate) fn ipi_interrupt(_cpu: CpuId) {
    todo!("SMP: mark this CPU's pending IPI work from the hardware callback")
}

/// 在安全边界处理并清空某 CPU 的 pending work。
pub(crate) fn drain_pending(_cpu: CpuId) {
    todo!("SMP: drain pending IPI work at a safe scheduling boundary")
}
