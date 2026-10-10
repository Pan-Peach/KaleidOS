//! IRQ 投递侧：外部中断入口 [`on_irq`]、路由 [`route`]，以及 irq-save
//! 临界区原语 [`IrqSaveGuard`]。
//!
//! # 结构约定
//!
//! - [`crate::resource::irq`]：IRQ route 真相（哪条线归哪个 owner、handler 是谁）
//!   与 register/enable/disable/release。
//! - 本模块：**投递**——后端拥有 ack/EOI 与源映射（claim 令牌、向量 / INTID →
//!   逻辑 IRQ 号都在 arch `InterruptController` 之后），Core 只收到一个**逻辑
//!   IRQ 号**并按 route 表投递；Core 不 claim、不 complete、不碰控制器寄存器。
//!
//! # C5 决策注记（irq-save 临界区）
//!
//! **irq-save 临界区**：中断可能在任意时刻打断持有 spin 锁的代码；被打断的
//! 上下文若持有锁，trap 处理器再取同样的锁 = 自死锁（spin 锁不可重入、单 CPU
//! 无人释放）。`CpuImpl::disable_irq() -> IrqFlags` + `restore_irq(flags)`：
//! 临界区进入时保存并关中断、退出时恢复。**preempt_count**（Linux 式）等真实
//! 工作量需要时再上。

use crate::component::{ComponentId, containment};
use crate::machine::CpuId;
use crate::resource::irq::IrqHandler;
use arch::{CpuArch, CpuImpl, InterruptController, InterruptImpl};

/// irq-save 临界区 guard：进入时保存并关中断，退出时恢复。
///
/// `_cpu_local` 让本 guard **不是 `Send`/`Sync`**：它保存的是**创建它的 CPU**
/// 的中断状态，跨 CPU / 跨线程 restore 会恢复错误的状态。
pub struct IrqSaveGuard {
    flags: Option<<CpuImpl as CpuArch>::IrqFlags>,
    _cpu_local: core::marker::PhantomData<*mut ()>,
}

impl IrqSaveGuard {
    pub fn new() -> Self {
        let flags = CpuImpl::disable_irq();
        Self {
            flags: Some(flags),
            _cpu_local: core::marker::PhantomData,
        }
    }

    /// Move restoration to the incoming execution, after its current/guard
    /// metadata and stack are installed. No RAII guard spans a context switch.
    pub(crate) fn into_flags(mut self) -> <CpuImpl as CpuArch>::IrqFlags {
        self.flags.take().expect("active IRQ guard")
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

/// 初始化外部中断投递：把 [`on_irq`] 注册为 arch 后端的分发目标。
///
/// `core::init` 在 `resource::init()` 之后调用一次。这里只**注册**回调，
/// 不打开 CPU 的全局中断使能位——开闸由第一条 IRQ 线的
/// [`crate::resource::irq::enable`] 触发（避免 boot 期无谓开闸）。
pub fn init() {
    InterruptImpl::register_external_handler(on_irq);
    // 本 CPU 的外部中断投递源（本地）；全局使能由 boot 在 `kernel::init` 之后
    // 用 `CpuArch::enable_irq` 单独负责。设备线 enable 不再碰投递源。
    let _ = <InterruptImpl as InterruptController>::init_cpu();
}

/// 初始化**当前执行 CPU** 的本地中断嵌套状态（AP 在本地启动时调用；UP 不调用）。
///
/// 本里程碑：外部 IRQ 固定路由到 BSP，AP 没有本地嵌套状态需要初始化——
/// irq-save 的中断状态由 `CpuArch::disable_irq` / `restore_irq` 直接承载，
/// `IrqSaveGuard` 本身已标记 non-`Send`/`Sync`（CPU-local）。
pub(crate) fn init_cpu(cpu: crate::machine::CpuId) -> Result<(), arch::smp::InitError> {
    if crate::smp::current_cpu() != cpu {
        return Err(arch::smp::InitError::InvalidConfiguration);
    }
    Ok(())
}

/// 外部中断入口（后端分发调用；中断上下文，已关中断）。
///
/// **单发**：后端拥有 `ack/claim → 源映射 → 本回调 → complete/EOI` 的循环，
/// 每一条已 ack 的中断调用本函数一次——Core 不循环、不 claim、不 complete。
/// `irq` 是后端映射出的**逻辑 IRQ 号**（不是 claim 令牌 / 向量 / INTID）。
///
/// 本函数只做路由：取投递目标 → 锁外调用组件 handler。一次 trap 里可能有多条
/// pending（PLIC 共享 mip 位）由后端循环处理。
///
/// handler 在 Core 建立的 **IRQ 归属作用域**内执行（[`dispatch_callback`]）：
/// principal = 该线 owner，`task = None`；作用域同步、不可 yield。
pub fn on_irq(_cpu: CpuId, irq: u32) {
    crate::trace::emit(crate::trace::TraceEvent::IrqEnter { irq });
    let target = prepare_callback(irq);
    let owner = target.map(|(owner, _, _)| owner);
    crate::trace::emit(crate::trace::TraceEvent::IrqDispatch {
        irq,
        component: owner,
    });
    if let Some((owner, handler, ctx)) = target {
        // 锁内只取一份拷贝，这里在锁外调用（trap 可重入，持锁调用组件代码
        // 会自死锁）。Core 用该线的 owner 建立 IRQ 归属作用域：回调内
        // `ambient()` 解析为 line owner、task = None。
        dispatch_callback(handler, ctx, owner);
        crate::component::registry::get_registry()
            .lock()
            .finish_call(owner);
    }
    crate::trace::emit(crate::trace::TraceEvent::IrqAck { irq });
}

/// Resolve and admit under registry → IRQ locks. Stop sees the callback count
/// even after its route is released. All locks are gone before running code.
fn prepare_callback(number: u32) -> Option<(ComponentId, IrqHandler, *mut ())> {
    let mut registry = crate::component::registry::get_registry().lock();
    let target = route(number)?;
    registry.begin_irq(target.0).ok()?;
    Some(target)
}

/// 在 Core 建立的 IRQ 归属作用域内调用一个组件回调：principal = 该中断线的
/// owner，`task = None`（IRQ 回调不是任务）。
///
/// 作用域由 [`crate::component::containment::with_irq_scope`] 安装/恢复，同步、
/// 不可 yield；因此调度类 Core 操作在回调内返回 errno 而不是 panic。它是
/// **可信 KernelNative 组件下的记账，不是认证边界**。
fn dispatch_callback(handler: IrqHandler, ctx: *mut (), owner: ComponentId) {
    containment::with_irq_scope(owner, || handler(ctx));
}

/// 把一条中断号路由成组件投递目标：`(owner, handler, ctx)`。
///
/// Core 真相：撤销 route 阻止后续准入；已复制并准入的 callback 仍可完成，
/// 由 component inflight 保活。**锁内只取拷贝，回调在锁外执行**。
pub fn route(number: u32) -> Option<(ComponentId, IrqHandler, *mut ())> {
    crate::resource::irq::get_table().lock().route_of(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::DeviceId;
    use crate::resource::irq::{self, IrqError};
    use crate::test_support::{Rank, TestLock};
    use core::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    /// 装一张固定 8 设备 × 每设备 1 资源的 IRQ 测试表（`get_table()` 的测试
    /// 覆盖槽）。
    ///
    /// **调用方必须持 [`crate::machine::test_support::GUARD`]**：资源表测试可能
    /// 用自己的 fixture 覆盖同一全局槽，MACHINE guard 把它们串行化。
    fn install_test_irq_table() {
        irq::install_for_test(&[1; 8]);
    }

    fn ready_owner() -> ComponentId {
        use crate::component::{endpoint::ExecutionDomain, registry};
        registry::init();
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(
                b"irq-owner",
                registry::test_support::test_loaded(0, None),
                ExecutionDomain::KernelNative,
            )
            .unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    /// Nested irq-save guards restore the state they observed, so only the outer
    /// guard that observed IRQs enabled may turn them back on.
    #[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
    #[test]
    fn nested_irq_save_guards_restore_the_outer_state() {
        assert!(arch::fake::irq_enabled_for_test());
        let outer = IrqSaveGuard::new();
        assert!(!arch::fake::irq_enabled_for_test());
        {
            let _inner = IrqSaveGuard::new();
            assert!(!arch::fake::irq_enabled_for_test());
        }
        assert!(!arch::fake::irq_enabled_for_test());
        drop(outer);
        assert!(arch::fake::irq_enabled_for_test());
    }

    /// 序列化触碰全局 IRQ 表的测试。
    ///
    /// rank = IRQ（模块本地、最外层；见 [`crate::test_support`]）。
    static IRQ_TEST_LOCK: TestLock = TestLock::new(Rank::Irq);

    extern "C" fn bump(_ctx: *mut ()) {
        CALLS.fetch_add(1, Ordering::AcqRel);
    }

    /// 验收：只对「已注册 route」的线给出投递目标；撤销后立刻停止。
    ///
    /// 投递走**生产路径**：host fake 后端的 [`arch::fake::deliver_external_for_test`]
    /// → `on_irq` → `route` → 锁外回调（不再是 claim 循环的 mock）。
    #[test]
    fn route_yields_delivery_only_for_registered_line() {
        let _serial = IRQ_TEST_LOCK.lock();
        let _boundary = crate::component::containment::test_boundary_lock();
        let _machine = crate::machine::test_support::GUARD.lock();
        crate::component::containment::enter_anchor();
        // 生产装配：route 表（resource，测试覆盖为固定 8 槽）+ 投递回调注册
        // （本模块 init → arch 后端）。
        install_test_irq_table();
        crate::irq::init();
        let owner = ready_owner();
        irq::get_table()
            .lock()
            .register(
                owner,
                DeviceId::from_raw(0),
                0,
                42,
                bump,
                core::ptr::null_mut(),
            )
            .unwrap();

        // 未注册 route 的线：无投递（中断到了也没人接）。
        arch::fake::deliver_external_for_test(CpuId::from_raw(0), 43);
        assert_eq!(CALLS.load(Ordering::Acquire), 0);

        // 注册的线：fake 后端回调 → 生产 `on_irq` → route → 锁外调用它。
        arch::fake::deliver_external_for_test(CpuId::from_raw(0), 42);
        assert_eq!(CALLS.load(Ordering::Acquire), 1);

        // 撤销后立刻停止投递。
        irq::get_table().lock().revoke_owner(owner);
        arch::fake::deliver_external_for_test(CpuId::from_raw(0), 42);
        assert_eq!(CALLS.load(Ordering::Acquire), 1);
        crate::component::containment::enter_anchor();
    }

    /// 验收：`release` 撤销该设备的 route。
    #[test]
    fn release_clears_the_route() {
        let _serial = IRQ_TEST_LOCK.lock();
        let _machine = crate::machine::test_support::GUARD.lock();
        install_test_irq_table();
        let owner = ComponentId::from_raw(0xbeef);
        irq::get_table()
            .lock()
            .register(
                owner,
                DeviceId::from_raw(5),
                0,
                43,
                bump,
                core::ptr::null_mut(),
            )
            .unwrap();
        assert!(route(43).is_some());
        assert_eq!(
            irq::get_table()
                .lock()
                .release(owner, DeviceId::from_raw(5), 0),
            Ok(())
        );
        assert!(route(43).is_none());
        assert_eq!(
            irq::get_table()
                .lock()
                .release(owner, DeviceId::from_raw(5), 0),
            Err(IrqError::NoHandler)
        );
    }

    #[test]
    fn admitted_callback_blocks_destroy_after_stop_and_route_release() {
        let _serial = IRQ_TEST_LOCK.lock();
        let _machine = crate::machine::test_support::GUARD.lock();
        install_test_irq_table();
        let owner = ready_owner();
        irq::get_table()
            .lock()
            .register(
                owner,
                DeviceId::from_raw(0),
                0,
                42,
                bump,
                core::ptr::null_mut(),
            )
            .unwrap();
        assert!(prepare_callback(42).is_some());
        irq::get_table().lock().revoke_owner(owner);
        let mut reg = crate::component::registry::get_registry().lock();
        reg.begin_stop(owner).unwrap();
        assert_eq!(
            reg.claim_destroy(owner),
            Err(crate::component::registry::RegistryError::Busy)
        );
        reg.finish_call(owner);
        reg.claim_destroy(owner).unwrap();
        drop(reg);
        irq::get_table()
            .lock()
            .register(
                owner,
                DeviceId::from_raw(0),
                0,
                42,
                bump,
                core::ptr::null_mut(),
            )
            .unwrap();
        assert!(prepare_callback(42).is_none());
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

    /// 验收：`route` 给出的 owner 就是回调内的 principal —— `on_irq` 的
    /// `dispatch_callback` 让 `ambient()` 解析为 line owner、`task = None`，
    /// 而不是被中断的执行。
    #[test]
    fn callback_dispatch_attributes_to_line_owner() {
        let _serial = IRQ_TEST_LOCK.lock();
        let _boundary = crate::component::containment::test_boundary_lock();
        let _machine = crate::machine::test_support::GUARD.lock();
        crate::component::containment::enter_anchor();
        // 生产装配：route 表（resource，测试覆盖为固定 8 槽）+ 投递回调注册
        // （本模块 init → arch 后端）。
        install_test_irq_table();
        crate::irq::init();
        let owner = ready_owner();
        irq::get_table()
            .lock()
            .register(
                owner,
                DeviceId::from_raw(0),
                0,
                42,
                observe_ambient,
                core::ptr::null_mut(),
            )
            .unwrap();
        OBSERVED_COMPONENT.store(usize::MAX, Ordering::Release);
        OBSERVED_TASK.store(usize::MAX, Ordering::Release);

        // 生产路径（不是直接调 `dispatch_callback`）：fake 后端回调 → `on_irq`
        // → `route`（Core truth 的 owner）→ 作用域内回调。
        arch::fake::deliver_external_for_test(CpuId::from_raw(0), 42);

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
}
