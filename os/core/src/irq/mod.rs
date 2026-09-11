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
/// revoke 后不会再有回调进入它的代码。handler 必须在**锁外**调用（trap 可能
/// 重入 spin 锁）。TODO(C6): 实现。
pub fn route(number: u32) -> bool {
    let _ = number;
    todo!("C6: IRQ 投递路由")
}
