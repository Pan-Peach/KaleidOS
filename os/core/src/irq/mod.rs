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

use crate::component::ComponentId;
use crate::handle::irq::{IrqDelivery, IrqHandler};
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
/// 循环从中断控制器取一条 pending 中断 → [`route`] 取得投递处置 →
/// 按处置执行（回调 inline / Polled 计数+掩蔽）→ `complete`。一次 trap 可能
/// 对应多条 pending（PLIC 共享一个 mip 位），必须循环到 claim 返回 `None`。
pub extern "C" fn on_external() {
    while let Some(line) = InterruptImpl::claim() {
        crate::trace::emit(crate::trace::TraceEvent::IrqEnter { irq: line });
        let outcome = route(line);
        let owner = match &outcome {
            RouteOutcome::Callback { owner, .. } | RouteOutcome::Polled { owner } => Some(*owner),
            RouteOutcome::None => None,
        };
        crate::trace::emit(crate::trace::TraceEvent::IrqDispatch {
            irq: line,
            component: owner,
        });
        match outcome {
            // Callback：锁内已取拷贝，这里在锁外直接调用（trap 可重入，
            // 持锁调用组件代码会自死锁）。
            RouteOutcome::Callback { handler, ctx, .. } => handler(ctx),
            // Polled：Core 计数；首个事件要求在锁外掩蔽该线，防止电平触发源
            // 在协作调度下反复打断。驱动任务随后 poll/ack 才重新放行。
            RouteOutcome::Polled { .. } => {
                let mask = {
                    let mut table = crate::handle::irq::get_table().lock();
                    table.note_polled_event(line)
                };
                if let Some(number) = mask {
                    InterruptImpl::disable(number);
                }
            }
            RouteOutcome::None => {}
        }
        crate::trace::emit(crate::trace::TraceEvent::IrqAck { irq: line });
        InterruptImpl::complete(line);
    }
}

/// [`route`] 的处置结果：锁内只读一份 Core 真相，实际动作在锁外执行。
pub enum RouteOutcome {
    /// 受信回调：在 trap 上下文锁外调用组件 handler。
    Callback {
        handler: IrqHandler,
        ctx: *mut (),
        /// 持有该线的组件（Core truth，trace 用）。
        owner: ComponentId,
    },
    /// 轮询投递：顶半部计数并（首事件）掩蔽该线。
    Polled {
        /// 持有该线的组件（Core truth，trace 用）。
        owner: ComponentId,
    },
    /// 该线无 owner / 未注册 delivery：什么都不做（仍 `complete`）。
    None,
}

/// 把一条中断号路由成处置结果。
///
/// Core 真相：只认 IRQ 表上「live slot + 已注册 delivery」的线——组件被
/// revoke 后不会再有回调进入它的代码。**锁内只取一份 (owner, delivery) 拷贝，
/// 实际投递动作（回调 / 控制器写）在锁外执行**（trap 可能重入 spin 锁）。
pub fn route(number: u32) -> RouteOutcome {
    let (owner, delivery) = {
        let table = crate::handle::irq::get_table().lock();
        match table.route_of(number) {
            Some(found) => found,
            None => return RouteOutcome::None,
        }
    };
    match delivery {
        Some(IrqDelivery::Callback { handler, ctx }) => {
            // SAFETY: `handler` 只由 `IrqDelivery::new` 从真实 `IrqHandler`
            // 写入（phase 1 信任 KernelNative 函数地址，同 schedule policy vtable）。
            let handler = unsafe { core::mem::transmute::<usize, IrqHandler>(handler) };
            RouteOutcome::Callback {
                handler,
                ctx: ctx as *mut (),
                owner,
            }
        }
        Some(IrqDelivery::Polled) => RouteOutcome::Polled { owner },
        None => RouteOutcome::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentId;
    use crate::handle::irq::{Irq, IrqDelivery};
    use core::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    /// 序列化触碰全局 IRQ 表的测试。两者都用 owner `0xfeed` 与中断号 7 的
    /// 全局 slot，且其中一个会 `revoke_owner(0xfeed)` —— 并发跑时会把对方的
    /// authority 撤掉（`set_polled` 的 `get_mut` 于是返回 Stale）。
    /// 沿用 sched / containment / trace 的锁纪律。
    static IRQ_TEST_LOCK: spin::Mutex<()> = spin::Mutex::new(());

    extern "C" fn bump(_ctx: *mut ()) {
        CALLS.fetch_add(1, Ordering::AcqRel);
    }

    /// 验收：只对「live slot + 已注册 delivery」的线给出投递处置；revoke 后立刻停止。
    #[test]
    fn route_yields_delivery_only_for_live_registered_owner() {
        let _serial = IRQ_TEST_LOCK.lock();
        crate::handle::irq::init();
        let owner = ComponentId::from_raw(0xfeed);
        let handle = crate::handle::irq::get_table()
            .lock()
            .grant(owner, Irq::new(42, 0));

        // 未注册 delivery：无处置（中断到了也没人接）
        assert!(matches!(route(42), RouteOutcome::None));
        assert_eq!(CALLS.load(Ordering::Acquire), 0);

        // 注册后得到 Callback 处置；on_external 会在锁外调用它
        crate::handle::irq::get_table()
            .lock()
            .set_delivery(owner, handle, IrqDelivery::new(bump, core::ptr::null_mut()))
            .unwrap();
        match route(42) {
            RouteOutcome::Callback { handler, ctx, .. } => handler(ctx),
            _ => panic!("expected Callback disposition"),
        }
        assert_eq!(CALLS.load(Ordering::Acquire), 1);

        // 没有 owner 的线：无处置
        assert!(matches!(route(43), RouteOutcome::None));
        assert_eq!(CALLS.load(Ordering::Acquire), 1);

        // revoke 后立刻停止投递（组件失败/卸载后回调进不去它的代码）
        crate::handle::irq::get_table().lock().revoke_owner(owner);
        assert!(matches!(route(42), RouteOutcome::None));
        assert_eq!(CALLS.load(Ordering::Acquire), 1);
    }

    /// 验收：Polled 线路由出 `RouteOutcome::Polled`（不产生回调）。
    #[test]
    fn route_yields_polled_for_polled_delivery() {
        let _serial = IRQ_TEST_LOCK.lock();
        crate::handle::irq::init();
        let owner = ComponentId::from_raw(0xfeed);
        let handle = crate::handle::irq::get_table()
            .lock()
            .grant(owner, Irq::new(7, 0));
        crate::handle::irq::get_table()
            .lock()
            .set_polled(owner, handle)
            .unwrap();

        assert!(matches!(route(7), RouteOutcome::Polled { .. }));
        // 顶半部处置：首事件计数并返回要掩蔽的中断号
        let number = crate::handle::irq::get_table().lock().note_polled_event(7);
        assert_eq!(number, Some(7));

        crate::handle::irq::get_table().lock().revoke_owner(owner);
    }

    // ------------------------------------------------------------------
    // irq-save guard + 外部中断入口（host 只验证接线层面，不碰全局 IRQ 表）
    // ------------------------------------------------------------------

    /// 验收：`IrqSaveGuard::default()`（走 `Default` impl → `new()`）能构造并析构。
    ///
    /// host 的 `arch::fake::Fake` 里 `disable_irq`/`restore_irq` 都是 no-op，
    /// 因此这里**不断言标志值**——irq-save 的真机语义由 Riscv 实现 + QEMU
    /// ArchTest 覆盖，host 只能证明该类型可构造/可析构。
    #[test]
    fn irq_save_guard_default_constructs_and_drops_on_host() {
        let guard = IrqSaveGuard::default();
        drop(guard);
        // 显式 `new()` 路径同样可构造/析构（Drop 会 take flags 后 restore）。
        let guard = IrqSaveGuard::new();
        drop(guard);
    }

    /// 验收：`crate::irq::init()` 可调用——host 上把 `on_external` 注册进 fake
    /// backend（`register_external_handler` 是 no-op）且不 panic。
    #[test]
    fn irq_init_registers_external_handler() {
        crate::irq::init();
    }

    /// host 的 `InterruptController::claim()` 恒返回 `None`，因此 `on_external`
    /// 的 claim→route→dispatch 循环体一次都不执行，调用应立即返回。
    ///
    /// 这里**只断言"调用返回、不 panic"**——绝不判定 body 行为：真实的
    /// claim/投递循环依赖真 PLIC 硬件，属于 QEMU ArchTest 的契约。
    #[test]
    fn on_external_returns_when_host_claim_is_none() {
        on_external();
    }
}
