//! IRQ 真相：中断号分配、mask、dispatch 归属。
//!
//! # 结构约定
//!
//! - [`crate::handle::irq`]：`IrqTable`（authority：哪条线归哪个 owner）+
//!   `claim` / `set_delivery` / `enable` 的 handle 校验。
//! - 本模块：**投递侧**——外部中断入口 [`on_external`]、路由 [`route`]，以及
//!   irq-save 临界区原语 [`IrqSaveGuard`]。控制器寄存器机制在 arch
//!   （`InterruptController` trait），Core 不碰 PLIC 寄存器。
//!
//! 实现时**按概念拆子模块、保持单文件小**（参考 `task/` 的粒度）——不预造空桩。
//!
//! # C5 决策注记（irq-save 临界区）
//!
//! **irq-save 临界区**：中断可能在任意时刻打断持有 spin 锁的代码；被打断的
//! 上下文若持有锁，trap 处理器再取同样的锁 = 自死锁（spin 锁不可重入、单 CPU
//! 无人释放）。两种纪律（选型 + 实现待人）：
//!
//! - **irq-save guard**（已落地）：`CpuImpl::disable_irq() -> IrqFlags` +
//!   `restore_irq(flags)`，临界区进入时保存并关中断、退出时恢复。
//!   简单、正确，教学项目推荐形态；表操作/调度的第一个消费点。
//! - **preempt_count**（Linux 式）：计数 > 0 时延迟抢占；更通用、规模更大，
//!   等真实工作量需要时再上。

use arch::{CpuArch, CpuImpl, InterruptController, InterruptImpl};

/// irq-save 临界区 guard：进入时保存并关中断，退出时恢复。
pub struct IrqSaveGuard {
    flags: Option<<CpuImpl as CpuArch>::IrqFlags>,
}

impl IrqSaveGuard {
    pub fn new() -> Self {
        let flags = CpuImpl::disable_irq();
        Self { flags: Some(flags) }
    }
}

impl Default for IrqSaveGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IrqSaveGuard {
    fn drop(&mut self) {
        if let Some(flags) = self.flags.take() {
            CpuImpl::restore_irq(flags);
        }
    }
}

/// 初始化外部中断投递：把 [`on_external`] 注册为 arch 的分发目标。
///
/// `core::init` 在 `handle::init()` 之后调用一次。这里只**注册**回调，
/// 不打开 CPU 的全局中断使能位——开闸由第一条 IRQ 线的
/// [`crate::handle::irq::enable`] 触发（避免 boot 期无谓开闸）。
pub fn init() {
    InterruptImpl::register_external_handler(on_external);
}

/// 外部中断入口（trap 分发调用；中断上下文，已关中断）。
///
/// 循环从中断控制器取一条 pending 中断 → [`route`] 投递给该线的 owner →
/// `complete`。一次 trap 可能对应多条 pending（PLIC 共享一个 mip 位），
/// 必须循环到 claim 返回 `None`。
pub extern "C" fn on_external() {
    while let Some(line) = InterruptImpl::claim() {
        route(line);
        InterruptImpl::complete(line);
    }
}

/// 把一条中断号投递给它的 owner 处理函数；返回是否已投递。
///
/// Core 真相：只认 IRQ 表上「live slot + 已注册 delivery」的线——组件被
/// revoke 后不会再有回调进入它的代码。**锁内只取一份 `IrqDelivery` 拷贝，
/// handler 在锁外调用**（trap 可能重入 spin 锁，持锁调用组件代码会自死锁）。
pub fn route(number: u32) -> bool {
    let delivery = {
        let table = crate::handle::irq::get_table().lock();
        table.delivery_of(number)
    };
    match delivery {
        Some(delivery) => {
            (delivery.handler())(delivery.ctx());
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentId;
    use crate::handle::irq::{Irq, IrqDelivery};
    use core::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn bump(_ctx: *mut ()) {
        CALLS.fetch_add(1, Ordering::AcqRel);
    }

    /// 验收：只投递给「live slot + 已注册 delivery」的 owner；revoke 后立刻停止。
    #[test]
    fn route_delivers_only_to_live_registered_owner() {
        crate::handle::irq::init();
        let owner = ComponentId::from_raw(0xfeed);
        let handle = crate::handle::irq::get_table().lock().grant(
            owner,
            Irq {
                number: 42,
                device_index: 0,
                delivery: None,
            },
        );

        // 未注册 delivery：不投递（中断到了也没人接）
        assert!(!route(42));
        assert_eq!(CALLS.load(Ordering::Acquire), 0);

        // 注册后可投递
        crate::handle::irq::get_table()
            .lock()
            .set_delivery(owner, handle, IrqDelivery::new(bump, core::ptr::null_mut()))
            .unwrap();
        assert!(route(42));
        assert_eq!(CALLS.load(Ordering::Acquire), 1);

        // 没有 owner 的线：不投递
        assert!(!route(43));
        assert_eq!(CALLS.load(Ordering::Acquire), 1);

        // revoke 后立刻停止投递（组件失败/卸载后回调进不去它的代码）
        crate::handle::irq::get_table().lock().revoke_owner(owner);
        assert!(!route(42));
        assert_eq!(CALLS.load(Ordering::Acquire), 1);
    }
}
