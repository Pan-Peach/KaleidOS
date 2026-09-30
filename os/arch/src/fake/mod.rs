use crate::cpu::{CpuId, HardwareCpuId};
use crate::smp::{CpuStartError, InitError, IpiError, LocalInterruptHandler, SecondaryBoot, Smp};
use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};

// 本 crate 整体 no_std；fake 仅在 host 编译（cfg 非 RV32/RV64），显式引入 std 供 console 直通。
extern crate std;
use std::{
    cell::{Cell, RefCell},
    io::Write,
    println,
    vec::Vec,
};

pub mod store;

/// Host 实现：console 直通 std stdout/stdin。
///
/// - `Console::write_byte`：逐字节写 stdout 并 flush——串口语义（无缓冲、保序），
///   让 `printk!`/`log!` 在 host 测试里真实可见（cargo test 会捕获到测试输出）。
/// - `Console::getc`：恒返回 `None`。`core::print::read_line()` 会忙等轮询，
///   host 测试中调用它会挂死——需要交互输入时走 QEMU 层，不在 fake 里读 stdin。
pub struct Fake;

// Host-only observable irq state: fake context switches do not change host stacks,
// but tests can still verify irq-save nesting and that a switch occurs with IRQs on.
std::thread_local! {
    static IRQ_ENABLED: Cell<bool> = const { Cell::new(true) };
    static LAST_SWITCH_IRQ_ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
}

// Host-only observable `tp`: the fake switch saves / restores it exactly like a
// real `__switch` (register #4 in `FakeContext`), so host tests can pin that `tp`
// is plain task execution state, preserved across switches.
std::thread_local! {
    static CURRENT_TP: Cell<usize> = const { Cell::new(0) };
}

// SMP 骨架：host 用一个线程本地绑定模拟「本 CPU 的 CPU-local 状态」。
// 真机语义（sscratch / GS / TPIDR_EL1 / KSAVE）见各 ISA 后端；host 只作为
// 可观察的占位，让 Core 的 per-CPU 骨架在 host 上可编译、可测试。
std::thread_local! {
    static CPU_ID: Cell<Option<usize>> = const { Cell::new(None) };
    static CPU_BASE: Cell<*mut ()> = const { Cell::new(core::ptr::null_mut()) };
    static SENT_IPIS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

// Fake timer 模拟（host 测试）：故障注入（init / delivery / arm）、单调 host
// tick、以及一条**有序事件日志**，让 Core 的「检查-睡眠」协议可被断言。
std::thread_local! {
    static TIMER_INIT_FAILS: Cell<bool> = const { Cell::new(false) };
    static TIMER_DELIVERY_FAILS: Cell<bool> = const { Cell::new(false) };
    static TIMER_ARM_FAILS: Cell<bool> = const { Cell::new(false) };
    static TIMER_DEADLINE: Cell<Option<u64>> = const { Cell::new(None) };
    static TIMER_EVENTS: RefCell<Vec<TimerEvent>> = const { RefCell::new(Vec::new()) };
    static HOST_TICKS: Cell<u64> = const { Cell::new(0) };
    static PENDING_WAKEUP: Cell<bool> = const { Cell::new(false) };
    static IDLE_SLEEPS: Cell<u64> = const { Cell::new(0) };
}

/// Fake 时钟 / idle 事件（host 测试可观察轨迹）。
///
/// 事件按发生顺序记录，让 Core 的「关中断 → arm → atomic_idle」协议可被
/// 精确断言（而不是只看最终状态）。
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerEvent {
    /// `set_deadline` 接受了 deadline；`irq_enabled` = 编程时模拟的中断使能
    /// 状态（idle 路径必须在**关中断**状态下 arm）。
    Armed { deadline: u64, irq_enabled: bool },
    /// `cancel_deadline`。
    Cancelled,
    /// `atomic_idle` 被进入；`saved_irq_enabled` = 调用者进入 idle 前的状态，
    /// `wakeup_pending` = 边界上是否已有 pending 唤醒。
    Idled {
        saved_irq_enabled: bool,
        wakeup_pending: bool,
    },
}

#[doc(hidden)]
pub fn timer_init_fails_for_test(fails: bool) {
    TIMER_INIT_FAILS.with(|flag| flag.set(fails));
}

#[doc(hidden)]
pub fn timer_delivery_fails_for_test(fails: bool) {
    TIMER_DELIVERY_FAILS.with(|flag| flag.set(fails));
}

#[doc(hidden)]
pub fn timer_arm_fails_for_test(fails: bool) {
    TIMER_ARM_FAILS.with(|flag| flag.set(fails));
}

/// 重置单调 host tick（测试可精确预期 deadline）。
#[doc(hidden)]
pub fn reset_host_clock_for_test() {
    HOST_TICKS.with(|ticks| ticks.set(0));
}

/// 模拟「唤醒在检查-睡眠边界上已经 pending」（例如 timer 恰好先到）。
/// `atomic_idle` 必须观察到它并返回，且**不消费**它。
#[doc(hidden)]
pub fn set_pending_wakeup_for_test(pending: bool) {
    PENDING_WAKEUP.with(|flag| flag.set(pending));
}

#[doc(hidden)]
pub fn pending_wakeup_for_test() -> bool {
    PENDING_WAKEUP.with(Cell::get)
}

/// `atomic_idle` 真正选择睡眠（无 pending 唤醒）的次数。
#[doc(hidden)]
pub fn idle_sleeps_for_test() -> u64 {
    IDLE_SLEEPS.with(Cell::get)
}

#[doc(hidden)]
pub fn timer_deadline_for_test() -> Option<u64> {
    TIMER_DEADLINE.with(Cell::get)
}

#[doc(hidden)]
pub fn take_timer_events_for_test() -> Vec<TimerEvent> {
    TIMER_EVENTS.with(|events| core::mem::take(&mut *events.borrow_mut()))
}

/// 已注册的全局 IPI 回调（进程级：注册是全局的，不随测试线程变）。
static IPI_HANDLER: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// 最近一次经 [`Fake`] 注册的 IPI 回调（host 无 IPI 传输，只作可观察占位）。
#[doc(hidden)]
pub fn registered_ipi_handler_for_test() -> Option<LocalInterruptHandler> {
    let address = IPI_HANDLER.load(core::sync::atomic::Ordering::Acquire);
    (address != 0).then(|| unsafe { core::mem::transmute::<usize, LocalInterruptHandler>(address) })
}

/// 取走并清空 host 记录的「已发出 IPI」目标硬件 id 列表（`Core::smp::ipi::notify` 可测）。
#[doc(hidden)]
pub fn take_sent_ipis_for_test() -> Vec<usize> {
    SENT_IPIS.with(|sent| core::mem::take(&mut *sent.borrow_mut()))
}

#[doc(hidden)]
pub fn irq_enabled_for_test() -> bool {
    IRQ_ENABLED.with(Cell::get)
}

#[doc(hidden)]
pub fn take_last_switch_irq_enabled_for_test() -> Option<bool> {
    LAST_SWITCH_IRQ_ENABLED.with(|enabled| enabled.replace(None))
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeContext {
    regs: [usize; 32],
    pc: usize,
}

impl FakeContext {
    pub const fn pc(&self) -> usize {
        self.pc
    }

    pub const fn reg(&self, idx: usize) -> usize {
        self.regs[idx]
    }
}

impl CpuArch for Fake {
    type Context = FakeContext;
    // 与 Riscv 同形（usize）；host 无真实中断状态，仅作占位。
    type IrqFlags = usize;

    fn context_switch(from: &mut Self::Context, to: &Self::Context) {
        LAST_SWITCH_IRQ_ENABLED.with(|last| last.set(Some(irq_enabled_for_test())));
        // Model the real `__switch`: save the running `tp` into the outgoing
        // record and load the incoming record's `tp` (x4 = tp, the same shape
        // as `RiscvContext`).  `tp` is plain execution state here.
        CURRENT_TP.with(|tp| from.regs[4] = tp.replace(to.regs[4]));
        println!("Switching context from {:?} to {:?}", from, to);
    }

    fn new_context(entry: usize, stack_top: usize) -> Self::Context {
        let mut ctx = Self::Context {
            regs: [0; 32],
            pc: entry,
        };
        ctx.regs[2] = stack_top; // sp
        ctx
    }

    fn init_cpu() {
        // host 无真实 trap 入口：no-op 占位。
    }

    fn enable_irq() {
        // host 无真实全局中断使能：把模拟状态置为启用，供 irq-save 测试观察。
        IRQ_ENABLED.with(|enabled| enabled.set(true));
    }

    fn disable_irq() -> Self::IrqFlags {
        // Host 没有真实中断，但模拟嵌套 save/restore 状态供 Core 测试断言。
        IRQ_ENABLED.with(|enabled| enabled.replace(false) as usize)
    }

    fn restore_irq(flags: Self::IrqFlags) {
        IRQ_ENABLED.with(|enabled| enabled.set(flags != 0));
    }

    fn wait_for_interrupt() {
        // host 无中断/时钟硬件：no-op（真机语义见 Riscv 实现）。
    }

    unsafe fn atomic_idle(flags: Self::IrqFlags) {
        // 可观察模拟：host 不真正睡眠，但记录边界状态，让 Core 测试断言
        // 「关中断 arm → idle」的协议与「pending 唤醒不丢」。
        let wakeup_pending = PENDING_WAKEUP.with(Cell::get);
        if !wakeup_pending {
            IDLE_SLEEPS.with(|sleeps| sleeps.set(sleeps.get() + 1));
        }
        TIMER_EVENTS.with(|events| {
            events.borrow_mut().push(TimerEvent::Idled {
                saved_irq_enabled: flags != 0,
                wakeup_pending,
            });
        });
        // 模拟「返回前恢复调用者保存的中断状态」。
        Self::restore_irq(flags);
    }

    fn current_cpu() -> Option<CpuId> {
        // host 恒为 CPU0（UP = 只有第 0 项的 SMP）；`install_per_cpu_base` 可覆盖。
        Some(CpuId::from_raw(CPU_ID.with(|id| id.get()).unwrap_or(0)))
    }

    fn per_cpu_base() -> Option<core::ptr::NonNull<()>> {
        CPU_BASE.with(|base| core::ptr::NonNull::new(base.get()))
    }

    unsafe fn install_per_cpu_base(cpu: CpuId, base: core::ptr::NonNull<()>) {
        // host 无真实入口记录：线程本地占位，供 Core per-CPU 骨架测试观察。
        CPU_ID.with(|id| id.set(Some(cpu.raw())));
        CPU_BASE.with(|slot| slot.set(base.as_ptr()));
    }
}

// SMP：host 没有真实次 CPU，`prepare` / `start_cpu` 保持 `todo!()`（host 测试不应
// 真的启动 CPU）。`register_ipi_handler` / `send_ipi*` 只做**可观察记录**（不真的
// 投递），让 Core 的 IPI 注册与 `notify` 发布路径可在 host 单测。
impl Smp for Fake {
    type BootConfig = ();

    unsafe fn prepare(_config: &'static Self::BootConfig) -> Result<(), InitError> {
        todo!("SMP: host fake has no secondary CPU startup")
    }

    unsafe fn start_cpu(
        _target: HardwareCpuId,
        _boot: &'static SecondaryBoot,
    ) -> Result<(), CpuStartError> {
        todo!("SMP: host fake has no secondary CPU startup")
    }

    fn init_ipi_cpu() -> Result<(), InitError> {
        Ok(())
    }

    fn register_ipi_handler(handler: LocalInterruptHandler) -> Result<(), InitError> {
        // Host 没有真实 IPI 传输，但 Core 的 `smp::init` 在 `cpu_count > 1` 时会
        // 注册回调；这里**记住**它而不是 `todo!()`，让 Core 的 SMP 骨架在 host 上
        // 可被单测。（真正投递见 `send_ipi*`：host 只做可观察记录。）
        IPI_HANDLER.store(handler as usize, core::sync::atomic::Ordering::Release);
        Ok(())
    }

    fn enable_ipi_interrupt() {}

    fn send_ipi(target: HardwareCpuId) -> Result<(), IpiError> {
        // Host 无 IPI 硬件：把目标记进可观察列表，让 Core 的 `notify` 可被测。
        SENT_IPIS.with(|sent| sent.borrow_mut().push(target.raw() as usize));
        Ok(())
    }

    fn send_ipi_mask(targets: &[HardwareCpuId]) -> Result<(), IpiError> {
        SENT_IPIS.with(|sent| {
            sent.borrow_mut()
                .extend(targets.iter().map(|t| t.raw() as usize));
        });
        Ok(())
    }
}

impl Timer for Fake {
    fn init_cpu() -> Result<(), crate::TimerError> {
        // 故障注入：测试可让"本地初始化失败"，验证 Core 不发布 readiness。
        if TIMER_INIT_FAILS.with(Cell::get) {
            return Err(crate::TimerError::Unsupported);
        }
        Ok(())
    }

    fn now() -> u64 {
        // 单调 host tick：每次读取前进 1（重置见 `reset_host_clock_for_test`）。
        HOST_TICKS.with(|ticks| {
            let value = ticks.get();
            ticks.set(value.wrapping_add(1));
            value
        })
    }

    fn set_deadline(deadline: u64) -> Result<(), crate::TimerError> {
        // 故障注入：测试可让 arm 失败，验证失败不发布 deadline 且不进入 idle。
        if TIMER_ARM_FAILS.with(Cell::get) {
            return Err(crate::TimerError::HardwareFailure);
        }
        TIMER_DEADLINE.with(|slot| slot.set(Some(deadline)));
        TIMER_EVENTS.with(|events| {
            events.borrow_mut().push(TimerEvent::Armed {
                deadline,
                irq_enabled: IRQ_ENABLED.with(Cell::get),
            });
        });
        Ok(())
    }

    fn cancel_deadline() {
        TIMER_DEADLINE.with(|slot| slot.set(None));
        TIMER_EVENTS.with(|events| events.borrow_mut().push(TimerEvent::Cancelled));
    }

    fn register_timer_handler(_handler: LocalInterruptHandler) {}

    fn enable_timer_interrupt() -> Result<(), crate::TimerError> {
        // 故障注入：模拟"投递不可用"（能力在、路由 / CPU interface 不在）。
        if TIMER_DELIVERY_FAILS.with(Cell::get) {
            return Err(crate::TimerError::DeliveryUnavailable);
        }
        Ok(())
    }
}

// host 无中断硬件：控制器全是 no-op，claim 恒 None（永远不会投递外部中断）。
impl InterruptController for Fake {
    type Config = ();
    type Claim = u32;

    unsafe fn configure(_config: ()) -> Result<(), InitError> {
        Ok(())
    }

    fn init_cpu() -> Result<(), InitError> {
        Ok(())
    }

    fn enable(_line: u32) {}
    fn disable(_line: u32) {}

    fn claim() -> Option<u32> {
        None
    }

    fn claim_line(claim: &u32) -> u32 {
        *claim
    }

    fn complete(_claim: u32) {}
    fn register_external_handler(_handler: LocalInterruptHandler) {}
    fn enable_external_interrupt() {}
}

impl Console for Fake {
    fn write_byte(byte: u8) {
        let mut out = std::io::stdout();
        let _ = out.write_all(&[byte]);
        let _ = out.flush();
    }

    fn getc() -> Option<u8> {
        None
    }
}

impl SystemReset for Fake {
    fn system_reset(_reset_type: ResetType) -> ! {
        panic!("fake system reset requested");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tp` 是**普通任务执行状态**：上下文切换精确保存 / 恢复它，example 哨兵值
    /// A→B→A 之后逐字恢复；切换路径从不把它当作组件身份或上下文线索。
    #[test]
    fn context_switch_restores_tp_exactly() {
        const SENTINEL_A: usize = 0x0A11_CE0A;
        const SENTINEL_B: usize = 0x0B0B_0B0B;

        let mut anchor = <Fake as CpuArch>::new_context(0, 0);
        let mut a = <Fake as CpuArch>::new_context(0x1000, 0x8000_0000);
        let mut b = <Fake as CpuArch>::new_context(0x2000, 0x9000_0000);
        a.regs[4] = SENTINEL_A;
        b.regs[4] = SENTINEL_B;

        // Given：从锚点切入 A；CPU 的 tp = A 记录里的哨兵值。
        Fake::context_switch(&mut anchor, &a);
        assert_eq!(CURRENT_TP.with(Cell::get), SENTINEL_A);

        // When：A → B。Then：tp = B；A 的记录保留自己的值。
        Fake::context_switch(&mut a, &b);
        assert_eq!(CURRENT_TP.with(Cell::get), SENTINEL_B);
        assert_eq!(a.regs[4], SENTINEL_A, "outgoing record keeps its own tp");

        // When：B → A。Then：tp 精确恢复为 A 的哨兵值；B 的记录保留自己的值。
        Fake::context_switch(&mut b, &a);
        assert_eq!(CURRENT_TP.with(Cell::get), SENTINEL_A);
        assert_eq!(b.regs[4], SENTINEL_B, "outgoing record keeps its own tp");

        // 再切回 B：两个哨兵都被逐字恢复，而不是碰巧等于初值 0。
        Fake::context_switch(&mut a, &b);
        assert_eq!(CURRENT_TP.with(Cell::get), SENTINEL_B);
    }
}
