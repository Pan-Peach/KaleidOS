//! IRQ 真相：中断号分配、mask、dispatch 归属。
//!
//! # 结构约定
//!
//! C6 起本模块会装下多个关注点（IrqTable = 分配/owner 真相、mask 管理、
//! dispatch 路由、IrqHandle 等）。实现时**按概念拆子模块、保持单文件小**
//! （参考 `task/` 的粒度）——不预造空桩，避免单文件巨无霸。
//!
//! # C5 决策注记（骨架，未实现）
//!
//! **irq-save 临界区**：中断可能在任意时刻打断持有 spin 锁的代码；被打断的
//! 上下文若持有锁，trap 处理器再取同样的锁 = 自死锁（spin 锁不可重入、单 CPU
//! 无人释放）。两种纪律（选型 + 实现待人）：
//!
//! - **irq-save guard**：`CpuImpl::disable_irq() -> IrqFlags` +
//!   `restore_irq(flags)`，临界区进入时保存并关中断、退出时恢复。
//!   简单、正确，教学项目推荐形态；sched 的 phase-1 临界区是第一个消费点。
//! - **preempt_count**（Linux 式）：计数 > 0 时延迟抢占；更通用、规模更大，
//!   等真实工作量需要时再上。
//!
//! 外部中断（PLIC/驱动 IRQ 分配与 dispatch）属于 C6，本模块暂不承载。

use arch::{CpuArch, CpuImpl};

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
