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
//! # 接通程度
//!
//! - **已机械实现**：[`ipi_interrupt`]（接收回调只标记）、[`take_pending`]
//!   （安全边界的原子 take）。
//! - **仍 `todo!()`（人类）**：[`notify`]（“先发布再响铃”的并发排序）与
//!   [`drain_pending`]（安全边界的重调度）。在 arch 补齐 **SSIP 应答**之前
//!   **不得**打开 IPI 源，否则会中断风暴（见 `.omo/plans/smp-production-integration.md`
//!   与 Oracle 评审）。
//!
//! 先只做实际需要的 `Reschedule`。**不**预先塞一个“无载荷 TLB shootdown 标志”
//! 就宣称完成：正确的 shootdown 需要受影响的地址空间/范围、确认与生存期规则。

use crate::machine::CpuId;
use crate::smp::{Backend, CpuMask, record};
use arch::smp::Smp;
use core::sync::atomic::Ordering;

/// [`IpiRequest::Reschedule`] 在 pending 位集里的位。
pub(crate) const RESCHEDULE_BIT: usize = 1;

/// 一次 IPI 请求（Core 语义；arch 只传门铃，不解释这些值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpiRequest {
    /// 请求目标 CPU 在安全边界重新调度。
    Reschedule,
}

impl IpiRequest {
    /// 在 pending 位集里的位掩码（人类实现 [`notify`] 时使用）。
    fn bit(self) -> usize {
        match self {
            IpiRequest::Reschedule => RESCHEDULE_BIT,
        }
    }
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
pub(crate) fn notify(targets: &CpuMask, request: IpiRequest) -> Result<(), NotifyError> {
    // 人类实现时的形状（不是机械接线，属并发关键逻辑）：
    //   1) 对每个有效目标先 `record.pending_ipi.fetch_or(request.bit(), Release)`；
    //   2) 收集有效目标的 `HardwareCpuId`，调 `Backend::send_ipi_mask`；
    //   3) 后端失败**保留** pending 位（不得回滚成“什么都没送到”）。
    // 在 arch 的 SSIP 应答与 `drain_pending` 落地前，本函数不得被启用。
    let mut hw = [arch::cpu::HardwareCpuId::from_raw(0); crate::machine::MAX_CPUS];
    let mut n = 0;
    for cpu in targets {
        let Some(rec) = record(cpu) else {
            return Err(NotifyError::InvalidTarget);
        };
        rec.pending_ipi.fetch_or(request.bit(), Ordering::Release);
        hw[n] = rec.hardware_id();
        n += 1;
    }
    <Backend as Smp>::send_ipi_mask(&hw[..n]).map_err(|_| NotifyError::DeliveryFailed)
}

/// 本 CPU 的 IPI 硬件回调（由 `arch::Smp::register_ipi_handler` 注册）。
///
/// 只标记 pending，不在此处做调度切换。
pub(crate) fn ipi_interrupt(cpu: CpuId) {
    if let Some(record) = crate::smp::record(cpu) {
        record
            .pending_ipi
            .fetch_or(RESCHEDULE_BIT, Ordering::Release);
    }
}

/// 在安全边界原子的取走某 CPU 的全部 pending work 位。
///
/// `swap(0)` 保证“取走”与“清除”不可分割：不与并发的 `ipi_interrupt` 丢更新。
/// 语义消费（`Reschedule → 重调度`）由人类实现的 [`drain_pending`] 承担。
pub(crate) fn take_pending(cpu: CpuId) -> usize {
    crate::smp::record(cpu).map_or(0, |record| record.pending_ipi.swap(0, Ordering::AcqRel))
}

/// 在安全边界处理并清空某 CPU 的 pending work。
///
/// 只做**不可分割的取走 + 意图落地**：`Reschedule` 转成该 CPU 的「需要重调度」
/// 标志（[`crate::smp::take_resched`]），真正的 context switch 由调度安全边界
/// （任务 yield/park/exit 后、或 AP 空闲循环顶部）消费标志后执行——绝不在 IPI
/// 硬件回调里切上下文。
pub(crate) fn drain_pending(cpu: CpuId) {
    let bits = take_pending(cpu);
    if bits & RESCHEDULE_BIT != 0
        && let Some(record) = crate::smp::record(cpu)
    {
        record.set_resched();
    }
}
