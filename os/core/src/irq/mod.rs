//! IRQ 投递侧：外部中断入口 [`on_external`]、路由 [`route`]，以及 irq-save
//! 临界区原语 [`IrqSaveGuard`]。
//!
//! # 结构约定
//!
//! - [`crate::resource::irq`]：IRQ route 真相（哪条线归哪个 owner、handler 是谁）
//!   与 register/enable/disable/release。
//! - 本模块：**投递**——Controller 寄存器机制在 arch
//!   （`InterruptController` trait），Core 不碰 PLIC 寄存器。
//!
//! # C5 决策注记（irq-save 临界区）
//!
//! **irq-save 临界区**：中断可能在任意时刻打断持有 spin 锁的代码；被打断的
//! 上下文若持有锁，trap 处理器再取同样的锁 = 自死锁（spin 锁不可重入、单 CPU
//! 无人释放）。`CpuImpl::disable_irq() -> IrqFlags` + `restore_irq(flags)`：
//! 临界区进入时保存并关中断、退出时恢复。**preempt_count**（Linux 式）等真实
//! 工作量需要时再上。

use crate::component::{ComponentId, containment};
use crate::resource::irq::IrqHandler;
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
/// `core::init` 在 `resource::init()` 之后调用一次。这里只**注册**回调，
/// 不打开 CPU 的全局中断使能位——开闸由第一条 IRQ 线的
/// [`crate::resource::irq::enable`] 触发（避免 boot 期无谓开闸）。
pub fn init() {
    InterruptImpl::register_external_handler(on_external);
}

/// 外部中断入口（trap 分发调用；中断上下文，已关中断）。
///
/// 循环从中断控制器取一条 pending 中断 → [`route`] 取投递目标 → 锁外调用
/// 组件 handler → `complete`。一次 trap 可能对应多条 pending（PLIC 共享一个
/// mip 位），必须循环到 claim 返回 `None`。
///
/// handler 在 Core 建立的 **IRQ 归属作用域**内执行（[`dispatch_callback`]）：
/// principal = 该线 owner，`task = None`；作用域同步、不可 yield。
pub extern "C" fn on_external() {
    while let Some(line) = InterruptImpl::claim() {
        crate::trace::emit(crate::trace::TraceEvent::IrqEnter { irq: line });
        let target = route(line);
        let owner = target.map(|(owner, _, _)| owner);
        crate::trace::emit(crate::trace::TraceEvent::IrqDispatch {
            irq: line,
            component: owner,
        });
        if let Some((owner, handler, ctx)) = target {
            // 锁内只取一份拷贝，这里在锁外调用（trap 可重入，持锁调用组件代码
            // 会自死锁）。Core 用该线的 owner 建立 IRQ 归属作用域：回调内
            // `ambient()` 解析为 line owner、task = None。
            dispatch_callback(handler, ctx, owner);
        }
        crate::trace::emit(crate::trace::TraceEvent::IrqAck { irq: line });
        InterruptImpl::complete(line);
    }
}

/// 在 Core 建立的 IRQ 归属作用域内调用一个组件回调：principal = 该中断线的
/// owner，`task = None`（IRQ 回调不是任务）。
///
/// 从 [`on_external`] 单独提出来，让 host 测试能走**生产路径**（host 的
/// `claim()` 恒为 `None`，永远进不了 `on_external` 的循环体）。作用域由
/// [`crate::component::containment::with_irq_scope`] 安装/恢复，同步、不可
/// yield；因此调度类 Core 操作在回调内返回 errno 而不是 panic。它是**可信
/// KernelNative 组件下的记账，不是认证边界**。
fn dispatch_callback(handler: IrqHandler, ctx: *mut (), owner: ComponentId) {
    containment::with_irq_scope(owner, || handler(ctx));
}

/// 把一条中断号路由成组件投递目标：`(owner, handler, ctx)`。
///
/// Core 真相：只认 IRQ 表上 live route——组件 release/失败后不会再有回调进入
/// 它的代码。**锁内只取一份拷贝，实际回调在锁外执行**（trap 可能重入 spin 锁）。
pub fn route(number: u32) -> Option<(ComponentId, IrqHandler, *mut ())> {
    crate::resource::irq::get_table().lock().route_of(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::irq::{self, IrqError};
    use crate::test_support::{Rank, TestLock};
    use core::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    /// 序列化触碰全局 IRQ 表的测试。
    ///
    /// rank = IRQ（模块本地、最外层；见 [`crate::test_support`]）。
    static IRQ_TEST_LOCK: TestLock = TestLock::new(Rank::Irq);

    extern "C" fn bump(_ctx: *mut ()) {
        CALLS.fetch_add(1, Ordering::AcqRel);
    }

    /// 验收：只对「已注册 route」的线给出投递目标；撤销后立刻停止。
    #[test]
    fn route_yields_delivery_only_for_registered_line() {
        let _serial = IRQ_TEST_LOCK.lock();
        let _boundary = crate::component::containment::test_boundary_lock();
        crate::component::containment::enter_anchor();
        irq::init();
        let owner = ComponentId::from_raw(0xfeed);
        irq::get_table()
            .lock()
            .register(owner, 0, 42, bump, core::ptr::null_mut());

        // 未注册 route 的线：无投递（中断到了也没人接）。
        assert!(route(43).is_none());
        assert_eq!(CALLS.load(Ordering::Acquire), 0);

        // 注册的线得到投递目标；on_external 会在锁外调用它。
        match route(42) {
            Some((routed, handler, ctx)) => {
                assert_eq!(routed, owner);
                handler(ctx);
            }
            None => panic!("expected a delivery target"),
        }
        assert_eq!(CALLS.load(Ordering::Acquire), 1);

        // 撤销后立刻停止投递。
        irq::get_table().lock().revoke_owner(owner);
        assert!(route(42).is_none());
        assert_eq!(CALLS.load(Ordering::Acquire), 1);
        crate::component::containment::enter_anchor();
    }

    /// 验收：`release` 撤销该设备的 route。
    #[test]
    fn release_clears_the_route() {
        let _serial = IRQ_TEST_LOCK.lock();
        irq::init();
        let owner = ComponentId::from_raw(0xbeef);
        irq::get_table()
            .lock()
            .register(owner, 5, 43, bump, core::ptr::null_mut());
        assert!(route(43).is_some());
        assert_eq!(irq::get_table().lock().release(owner, 5), Ok(()));
        assert!(route(43).is_none());
        assert_eq!(
            irq::get_table().lock().release(owner, 5),
            Err(IrqError::NoHandler)
        );
    }

    // ------------------------------------------------------------------
    // IRQ 归属：回调在 Core 建立的 line-owner 作用域内执行
    // ------------------------------------------------------------------

    static OBSERVED_COMPONENT: AtomicUsize = AtomicUsize::new(usize::MAX);
    static OBSERVED_TASK: AtomicUsize = AtomicUsize::new(usize::MAX);

    fn ambient_component_raw() -> usize {
        crate::resource::RequestContext::ambient()
            .map_or(usize::MAX, |ctx| ctx.component.raw() as usize)
    }

    extern "C" fn observe_ambient(_ctx: *mut ()) {
        let ambient = crate::resource::RequestContext::ambient();
        OBSERVED_COMPONENT.store(ambient_component_raw(), Ordering::Release);
        OBSERVED_TASK.store(
            ambient.as_ref().is_some_and(|ctx| ctx.task.is_some()) as usize,
            Ordering::Release,
        );
    }

    /// 验收：`route` 给出的 owner 就是回调内的 principal —— `dispatch_callback`
    /// 让 `ambient()` 解析为 line owner、`task = None`，而不是被中断的执行。
    #[test]
    fn callback_dispatch_attributes_to_line_owner() {
        let _serial = IRQ_TEST_LOCK.lock();
        let _boundary = crate::component::containment::test_boundary_lock();
        crate::component::containment::enter_anchor();
        irq::init();
        let owner = ComponentId::from_raw(0xfeed);
        irq::get_table()
            .lock()
            .register(owner, 0, 42, observe_ambient, core::ptr::null_mut());
        OBSERVED_COMPONENT.store(usize::MAX, Ordering::Release);
        OBSERVED_TASK.store(usize::MAX, Ordering::Release);

        let (routed, handler, ctx) = route(42).expect("route");
        assert_eq!(routed, owner, "route yields the Core-truth line owner");
        dispatch_callback(handler, ctx, routed);

        assert_eq!(
            OBSERVED_COMPONENT.load(Ordering::Acquire),
            owner.raw() as usize,
            "principal inside the callback is the IRQ line's owner"
        );
        assert_eq!(
            OBSERVED_TASK.load(Ordering::Acquire),
            0,
            "an IRQ callback carries no task"
        );
        assert!(
            !crate::component::containment::in_irq_context(),
            "scope is restored after the callback"
        );

        crate::component::containment::enter_anchor();
        irq::get_table().lock().revoke_owner(owner);
    }

    /// 验收：`IrqSaveGuard::default()`（走 `Default` impl → `new()`）能构造并析构。
    #[test]
    fn irq_save_guard_default_constructs_and_drops_on_host() {
        let guard = IrqSaveGuard::default();
        drop(guard);
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
    #[test]
    fn on_external_returns_when_host_claim_is_none() {
        on_external();
    }
}
