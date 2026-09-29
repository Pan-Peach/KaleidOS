use crate::cpu::{CpuId, HardwareCpuId};
use crate::smp::{CpuStartError, InitError, IpiError, LocalInterruptHandler, SecondaryBoot, Smp};
use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};

// 本 crate 整体 no_std；fake 仅在 host 编译（cfg 非 RV32/RV64），显式引入 std 供 console 直通。
extern crate std;
use std::{cell::Cell, io::Write, println};

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

// SMP 骨架：host 用一个线程本地绑定模拟「本 CPU 的 CPU-local 状态」。
// 真机语义（sscratch / GS / TPIDR_EL1 / KSAVE）见各 ISA 后端；host 只作为
// 可观察的占位，让 Core 的 per-CPU 骨架在 host 上可编译、可测试。
std::thread_local! {
    static CPU_ID: Cell<Option<usize>> = const { Cell::new(None) };
    static CPU_BASE: Cell<*mut ()> = const { Cell::new(core::ptr::null_mut()) };
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
        // Placeholder for context switch logic
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

    fn runtime_slot() -> usize {
        // host 无真实寄存器 / 无真实执行：没有「当前执行的 tp」，恒为 0（无 slot）。
        0
    }

    fn install_runtime_slot(_slot: usize) {
        // host 无真实寄存器：no-op 占位（真机语义见 Riscv 实现）。
    }

    fn set_context_slot(context: &mut Self::Context, slot: usize) {
        // x4 = tp（与 Riscv 的上下文记录同形）；host 只作为测试可观察的占位。
        context.regs[4] = slot;
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

// SMP 骨架：host 没有真实次 CPU / IPI 硬件。方法体一律 `todo!()`，因为任何
// host 测试都不应真的启动 CPU 或发 IPI；它们的存在只是让 `SmpImpl` 在 host 上
// 满足 trait bound，并让 Core 的 smp 骨架可编译。
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

    fn init_cpu() -> Result<(), InitError> {
        Ok(())
    }

    fn register_ipi_handler(_handler: LocalInterruptHandler) -> Result<(), InitError> {
        todo!("SMP: host fake has no IPI transport")
    }

    fn enable_ipi_interrupt() {}

    fn send_ipi(_target: HardwareCpuId) -> Result<(), IpiError> {
        todo!("SMP: host fake has no IPI transport")
    }

    fn send_ipi_mask(_targets: &[HardwareCpuId]) -> Result<(), IpiError> {
        todo!("SMP: host fake has no IPI transport")
    }
}

impl Timer for Fake {
    fn init_cpu() -> Result<(), InitError> {
        Ok(())
    }

    fn now() -> u64 {
        // host 占位时间源（C5 骨架）：非单调 0，仅供编译/接线占位。
        0
    }

    fn set_deadline(_deadline: u64) {
        // host 无定时器硬件：no-op 占位。
    }

    fn cancel_deadline() {}

    fn register_timer_handler(_handler: LocalInterruptHandler) {}

    fn enable_timer_interrupt() {}
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
