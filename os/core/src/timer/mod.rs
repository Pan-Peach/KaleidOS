//! Timer 真相：定时器句柄、到期回调归属。
//!
//! # 结构约定
//!
//! 本模块预期持续成长（sleep/超时/按组件的 `TimerHandle` 等）。实现时**按概念
//! 拆子模块、保持单文件小**（参考 `task/` 的粒度：id/state/table/error 各一个
//! 文件）——不预造空桩，等第一个真实关注点出现时落文件，避免单文件巨无霸。
//!
//! # 定位（Core 机制，canonical，不做成组件）
//!
//! 单次 deadline 编程 + tick 分发 + 与调度器的抢占 seam。硬件访问走
//! `arch::TimerImpl`（`Timer` trait：`now` / `set_deadline`），Core 不感知
//! SBI/CLINT 细节；当前只有 Core 自己消费（组件 `TimerHandle` 未实现）。
//!
//! # per-CPU（SMP）
//!
//! `TimerState` 每逻辑 CPU 一份（[`crate::smp::PerCpu`]）。硬件已是本地的
//! （SBI `set_timer` 作用于调用 hart；M-mode 用 `mhartid` 选 `mtimecmp`），
//! per-CPU 软件状态补上 Core 缺失的真相：**一台 CPU 的 trap 不再推进/清除
//! 另一台 CPU 的 `next_deadline` / `ticks`**。UP = 只有第 0 项的 SMP。
//!
//! # 接线点
//!
//! 1. `init`：在当前 CPU 上登记 trap 回调并解开本 CPU timer 源，不自动产生周期 tick；
//! 2. `arm_deadline`：为本 CPU 的下一个 sleep/timeout/event 编程 one-shot deadline；
//! 3. `init_cpu`：AP 在本地启动时初始化本 CPU（必须由该 CPU 自己调用）；
//! 4. 中断开闸：`sie.STIE` + `sstatus.SIE`（`CpuImpl` 侧原语）；
//! 5. 可选抢占：仅 `preempt` profile 由 `init_preempt` 使用周期 deadline。
use crate::machine::CpuId;
use arch::Timer;
use spin::{Mutex, Once};

struct TimerState {
    /// 本 CPU 的 timer **投递链路**是否端到端就绪：后端本地初始化 + 投递激活
    /// 都成功后才发布（见 [`init_cpu`]）。单一 readiness 字段，不是状态机。
    delivery_ready: bool,
    ticks: u64,
    next_deadline: Option<u64>,
    #[cfg(feature = "preempt")]
    preempt_period: Option<u64>,
}

impl TimerState {
    const fn new() -> Self {
        Self {
            delivery_ready: false,
            ticks: 0,
            next_deadline: None,
            #[cfg(feature = "preempt")]
            preempt_period: None,
        }
    }
}

/// 每逻辑 CPU 一份 timer 软件真相（懒构造，容量 = `MAX_CPUS`）。
static STATE: Once<crate::smp::PerCpu<Mutex<TimerState>>> = Once::new();

fn table() -> &'static crate::smp::PerCpu<Mutex<TimerState>> {
    STATE.call_once(|| {
        crate::smp::PerCpu::new(crate::machine::MAX_CPUS, |_| Mutex::new(TimerState::new()))
            .expect("timer per-cpu table allocation failed")
    });
    STATE.get().expect("timer per-cpu table just initialized")
}

#[cfg(feature = "preempt")]
const PREEMPT_HZ: u64 = 100;

/// 时钟机制错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerError {
    /// 频率非法（0 或换算溢出）。
    InvalidFrequency,
    /// 已经初始化（Core 单例机制，只 init 一次）。
    AlreadyInitialized,
    /// 本 CPU 的 timer 投递未就绪（`on_trap` / `arm_deadline` / `ticks` 先于
    /// `init` 或后端失败之后）。
    NotInitialized,
    /// 后端失败（本地初始化 / 投递激活 / deadline 编程），原始原因保留。
    Backend(arch::TimerError),
    /// [`init_cpu`] 只能在其目标 CPU 上本地执行。
    WrongCpu,
}

/// 初始化**当前执行 CPU** 的 one-shot timer 机制：登记时钟回调并只解本 CPU
/// timer 源。不自动编程 deadline（cooperative profile 因此没有周期 IRQ）。
///
/// `init` 是 BSP 的便捷入口；AP 在 `secondary_entry` 里调用 [`init_cpu`]。
/// 失败表示本 CPU 没有可用的 timer 投递；调用者（`core::init` / AP 入口）
/// 据此回退到轮询 idle 或 fail-closed。
pub fn init() -> Result<(), TimerError> {
    let cpu = crate::smp::current_cpu();
    let slot = table()
        .get(cpu)
        .expect("current cpu index within timer capacity");
    if slot.lock().delivery_ready {
        return Err(TimerError::AlreadyInitialized);
    }
    arch::TimerImpl::register_timer_handler(on_trap);
    init_cpu(cpu)
}

/// 初始化**指定 CPU**（必须是调用者本人）的本地 timer：后端本地初始化 +
/// 投递激活。`delivery_ready` 只在**两者都成功后**发布（此前是提前置位）。
pub(crate) fn init_cpu(cpu: CpuId) -> Result<(), TimerError> {
    if crate::smp::current_cpu() != cpu {
        return Err(TimerError::WrongCpu);
    }
    let slot = table().get(cpu).expect("cpu index within timer capacity");
    <arch::TimerImpl as Timer>::init_cpu().map_err(TimerError::Backend)?;
    // 只解本 CPU 的 timer 源；**投递链路端到端可用**才允许返回 Ok（Core 据
    // 此发布 readiness；失败则绝不假装成功）。
    <arch::TimerImpl as Timer>::enable_timer_interrupt().map_err(TimerError::Backend)?;
    slot.lock().delivery_ready = true;
    Ok(())
}

/// 本 CPU 的 timer 投递是否已就绪；**不分配、不初始化**（`STATE.get()`），
/// 供早期 boot / 轮询回退路径在分配器就绪前安全查询。
pub(crate) fn delivery_ready() -> bool {
    let cpu = crate::smp::current_cpu();
    STATE
        .get()
        .and_then(|table| table.get(cpu))
        .is_some_and(|slot| slot.lock().delivery_ready)
}

/// 为**当前 CPU** 编程下一次 one-shot deadline。
///
/// 检查、硬件编程与软件状态发布都在**关中断**下进行；只有硬件真的接受
/// deadline（`Ok`）后才发布新的 `next_deadline`——失败不发布。
pub fn arm_deadline(deadline: u64) -> Result<(), TimerError> {
    let _irq_guard = crate::irq::IrqSaveGuard::new();
    let cpu = crate::smp::current_cpu();
    // 预初始化路径不得分配（`table()` 会懒分配）：无表 = 未就绪。
    let Some(slot) = STATE.get().and_then(|table| table.get(cpu)) else {
        return Err(TimerError::NotInitialized);
    };
    let mut state = slot.lock();
    if !state.delivery_ready {
        return Err(TimerError::NotInitialized);
    }
    <arch::TimerImpl as Timer>::set_deadline(deadline).map_err(TimerError::Backend)?;
    state.next_deadline = Some(deadline);
    Ok(())
}

#[cfg(feature = "preempt")]
/// 为抢占 profile 在**当前 CPU** 上初始化周期性调度 tick。
pub fn init_preempt(timebase_hz: usize) -> Result<(), TimerError> {
    let timebase_hz = u64::try_from(timebase_hz).map_err(|_| TimerError::InvalidFrequency)?;
    if timebase_hz < PREEMPT_HZ {
        return Err(TimerError::InvalidFrequency);
    }
    let period = timebase_hz / PREEMPT_HZ;
    if period == 0 {
        return Err(TimerError::InvalidFrequency);
    }
    let first_deadline = arch::TimerImpl::now()
        .checked_add(period)
        .ok_or(TimerError::InvalidFrequency)?;
    init()?;
    {
        let cpu = crate::smp::current_cpu();
        table()
            .get(cpu)
            .expect("current cpu index within timer capacity")
            .lock()
            .preempt_period = Some(period);
    }
    arm_deadline(first_deadline)
}

/// 时钟中断入口（trap 分发调用；中断上下文，已关中断）。
///
/// `cpu` 是 arch 传下的**逻辑**身份；只作用于该 CPU 自己的槽位。职责：重编程
/// 下一次 deadline + tick 计数 + 触发调度抢占 seam（`crate::sched::on_timer_tick`）。
///
/// 抢占模型（延迟重调度 vs trap 内直接切换）见 [`crate::sched::on_timer_tick`]。
pub fn on_trap(cpu: CpuId) {
    // 预初始化 trap 不得分配（`table()` 会懒分配）：无表 = 未就绪，直接返回。
    let Some(slot) = STATE.get().and_then(|table| table.get(cpu)) else {
        return;
    };
    let mut state = slot.lock();
    if !state.delivery_ready {
        return;
    }
    state.ticks += 1;
    #[cfg(feature = "preempt")]
    if let Some(period) = state.preempt_period {
        let now = arch::TimerImpl::now();
        let mut next = state.next_deadline.unwrap_or(now);
        while next <= now {
            next = next.saturating_add(period);
        }
        // 只有硬件接受才发布新 deadline；失败则清空软件真相。
        if <arch::TimerImpl as Timer>::set_deadline(next).is_ok() {
            state.next_deadline = Some(next);
        } else {
            state.next_deadline = None;
        }
        return;
    }
    #[cfg(not(feature = "preempt"))]
    {
        // one-shot：这次 trap 已消费本 CPU 的 deadline。
        state.next_deadline = None;
        <arch::TimerImpl as Timer>::cancel_deadline();
    }
}

/// **当前 CPU** 已过去的 tick 数（观测/测试用）。
pub fn ticks() -> u64 {
    let _irq_guard = crate::irq::IrqSaveGuard::new();
    let cpu = crate::smp::current_cpu();
    STATE
        .get()
        .and_then(|table| table.get(cpu))
        .map_or(0, |slot| slot.lock().ticks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Rank, TestLock};

    /// 序列化触碰进程级 timer 全局的测试。
    ///
    /// `STATE` 是进程级 `Once`，`init()` 每个 CPU 只能成功一次且无法重置，
    /// 所以整条生命周期必须放在单个 `#[test]` 里。锁本身沿用 irq / sched /
    /// containment / trace 的纪律，防止新增测试并发改动同一全局。
    ///
    /// rank = TIMER（模块本地、最外层；见 [`crate::test_support`]）。
    static TIMER_TEST_LOCK: TestLock = TestLock::new(Rank::Timer);

    /// 安装一份 10 MHz timebase 的机器 fixture（idle 周期换算用）。
    ///
    /// 调用方必须持有 [`crate::machine::test_support::GUARD`]。
    fn install_timebase(timebase_frequency: u64) {
        let info = crate::machine::test_support::snapshot(
            crate::machine::HardwareCpuId::from_raw(0),
            timebase_frequency,
            alloc::vec![crate::machine::CpuInfo {
                boot_cpu: true,
                hardware_id: crate::machine::HardwareCpuId::from_raw(0),
            }],
            alloc::vec![crate::machine::MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }],
            alloc::vec![],
        );
        crate::machine::test_support::install(info);
    }

    fn cpu0() -> &'static Mutex<TimerState> {
        table().get(CpuId::from_raw(0)).unwrap()
    }

    /// 验收：one-shot timer 机制的完整生命周期 —— readiness 发布规则、
    /// 预初始化路径不分配、失败 arm 不睡眠、检查→idle 协议（关中断 arm、
    /// 保存状态恢复、边界上不丢 pending 唤醒）、trap 计数与 one-shot 清除。
    ///
    /// 进程级 `Once` 只能 init 一次，所以整条生命周期（含失败注入重试）都在
    /// 单个测试里；`arch::fake` 提供确定性故障注入与有序事件日志。
    #[test]
    fn timer_lifecycle_covers_readiness_idle_protocol_and_ticks() {
        let _serial = TIMER_TEST_LOCK.lock();
        // MACHINE rank(1) > TIMER rank(-1)：顺序合法。
        let _machine = crate::machine::test_support::GUARD.lock();
        install_timebase(10_000_000);

        // Given: 没有任何 timer 表；后端本地初始化被注入失败。
        assert!(
            STATE.get().is_none(),
            "no other test may pre-allocate the table"
        );
        assert!(!delivery_ready());
        arch::fake::timer_init_fails_for_test(true);

        // When: 预初始化路径上编程 deadline / 收到 trap / 进入 idle。
        let armed = arm_deadline(123);
        on_trap(CpuId::from_raw(0));
        crate::print::idle_wait();

        // Then: 全部被拒绝，**不分配、不 arm、不睡眠**。
        assert_eq!(armed, Err(TimerError::NotInitialized));
        assert_eq!(ticks(), 0);
        assert!(
            STATE.get().is_none(),
            "pre-init paths must not allocate the timer table"
        );
        assert!(arch::fake::take_timer_events_for_test().is_empty());
        assert_eq!(arch::fake::idle_sleeps_for_test(), 0);

        // When: 后端本地初始化失败时 init。
        let failed = init();

        // Then: 后端错误原样上报，且**不发布 readiness**（失败可重试）。
        assert_eq!(
            failed,
            Err(TimerError::Backend(arch::TimerError::Unsupported))
        );
        assert!(!delivery_ready());

        // Given: 后端本地初始化恢复，但投递路径不可用。
        arch::fake::timer_init_fails_for_test(false);
        arch::fake::timer_delivery_fails_for_test(true);

        // When: 再次 init。
        let no_delivery = init();

        // Then: 投递失败原样上报，readiness 仍不发布。
        assert_eq!(
            no_delivery,
            Err(TimerError::Backend(arch::TimerError::DeliveryUnavailable))
        );
        assert!(!delivery_ready());

        // Given: 投递恢复，但 deadline 编程失败。
        arch::fake::timer_delivery_fails_for_test(false);
        arch::fake::timer_arm_fails_for_test(true);

        // When: 第三次 init（两次失败都没有发布 readiness，仍可重试）。
        let first = init();

        // Then: 成功并发布 readiness；单例机制拒绝第二次 init。
        assert_eq!(first, Ok(()));
        assert!(delivery_ready());
        assert_eq!(init(), Err(TimerError::AlreadyInitialized));

        // When: arm 失败时进入 idle。
        crate::print::idle_wait();

        // Then: 不 arm、不睡眠；保存的中断状态被恢复。
        assert!(arch::fake::take_timer_events_for_test().is_empty());
        assert_eq!(arch::fake::idle_sleeps_for_test(), 0);
        assert!(
            arch::fake::irq_enabled_for_test(),
            "saved IRQ state must be restored on the polling fallback"
        );

        // Given: arm 成功；host clock 归零（deadline 可精确预期）。
        arch::fake::timer_arm_fails_for_test(false);
        arch::fake::reset_host_clock_for_test();

        // When: 进入 idle。
        crate::print::idle_wait();

        // Then: 唤醒在**关中断**状态下先建立，随后带着调用者保存的中断状态
        // 进入 idle；返回后状态恢复、deadline 保持已编程。
        assert_eq!(
            arch::fake::take_timer_events_for_test(),
            alloc::vec![
                arch::fake::TimerEvent::Armed {
                    deadline: 100_000,
                    irq_enabled: false,
                },
                arch::fake::TimerEvent::Idled {
                    saved_irq_enabled: true,
                    wakeup_pending: false,
                },
            ]
        );
        assert_eq!(arch::fake::idle_sleeps_for_test(), 1);
        assert!(arch::fake::irq_enabled_for_test());
        assert_eq!(arch::fake::timer_deadline_for_test(), Some(100_000));

        // When: 唤醒恰好在检查→idle 边界上已经 pending（timer 先到）。
        arch::fake::set_pending_wakeup_for_test(true);
        arch::fake::reset_host_clock_for_test();
        crate::print::idle_wait();

        // Then: idle 观察到它、选择不睡眠、且**不消费**它（不丢唤醒）。
        assert_eq!(
            arch::fake::take_timer_events_for_test(),
            alloc::vec![
                arch::fake::TimerEvent::Armed {
                    deadline: 100_000,
                    irq_enabled: false,
                },
                arch::fake::TimerEvent::Idled {
                    saved_irq_enabled: true,
                    wakeup_pending: true,
                },
            ]
        );
        assert!(
            arch::fake::pending_wakeup_for_test(),
            "a wakeup pending at the check→idle boundary must not be lost"
        );
        assert_eq!(arch::fake::idle_sleeps_for_test(), 1);
        arch::fake::set_pending_wakeup_for_test(false);

        // When: init 之后编程 deadline。
        let armed = arm_deadline(500);

        // Then: 这次被接受并记录（One-shot 语义：只等这一次）。
        assert_eq!(armed, Ok(()));
        assert_eq!(cpu0().lock().next_deadline, Some(500));

        // When: 时钟 trap 连续到达 3 次。
        on_trap(CpuId::from_raw(0));
        on_trap(CpuId::from_raw(0));
        on_trap(CpuId::from_raw(0));

        // Then: 每次 trap 都推进 tick 计数。
        assert_eq!(ticks(), 3);

        // When/Then: 默认（非 preempt）profile 下 on_trap 清除待处理的
        // deadline（one-shot 语义），后续 trap 继续计数。
        #[cfg(not(feature = "preempt"))]
        assert_eq!(cpu0().lock().next_deadline, None);
        on_trap(CpuId::from_raw(0));
        assert_eq!(ticks(), 4);

        // When: 跨 CPU 调用本 CPU 的本地初始化。
        let wrong_cpu = init_cpu(CpuId::from_raw(1));

        // Then: 被拒，且远端槽位不得变成 ready。
        assert_eq!(
            wrong_cpu,
            Err(TimerError::WrongCpu),
            "an AP's timer must be initialized by that AP, not remotely"
        );
        assert!(
            !table()
                .get(CpuId::from_raw(1))
                .unwrap()
                .lock()
                .delivery_ready
        );
    }
}
