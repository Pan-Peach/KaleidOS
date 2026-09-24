use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};

// 本 crate 整体 no_std；fake 仅在 host 编译（cfg 非 RV32/RV64），显式引入 std 供 console 直通。
extern crate std;
use std::{io::Write, println};

pub mod store;

/// Host 实现：console 直通 std stdout/stdin。
///
/// - `Console::write_byte`：逐字节写 stdout 并 flush——串口语义（无缓冲、保序），
///   让 `printk!`/`log!` 在 host 测试里真实可见（cargo test 会捕获到测试输出）。
/// - `Console::getc`：恒返回 `None`。`core::print::read_line()` 会忙等轮询，
///   host 测试中调用它会挂死——需要交互输入时走 QEMU 层，不在 fake 里读 stdin。
pub struct Fake;

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

    fn init() {
        // host 无真实 trap 入口：no-op 占位。
    }

    fn disable_irq() -> Self::IrqFlags {
        // host 无真实中断：no-op 占位（irq-save 语义在真机上由 Riscv 实现）。
        0
    }

    fn restore_irq(_flags: Self::IrqFlags) {
        // host 无真实中断：no-op 占位。
    }

    fn wait_for_interrupt() {
        // host 无中断/时钟硬件：no-op（真机语义见 Riscv 实现）。
    }
}

impl Timer for Fake {
    fn now() -> u64 {
        // host 占位时间源（C5 骨架）：非单调 0，仅供编译/接线占位。
        0
    }

    fn set_deadline(_deadline: u64) {
        // host 无定时器硬件：no-op 占位。
    }

    fn cancel_deadline() {}

    fn register_timer_handler(_handler: extern "C" fn()) {}

    fn enable_timer_interrupt() {}
}

// host 无中断硬件：控制器全是 no-op，claim 恒 None（永远不会投递外部中断）。
impl InterruptController for Fake {
    fn configure(_base: usize, _hart_id: usize) {}
    fn enable(_line: u32) {}
    fn disable(_line: u32) {}
    fn claim() -> Option<u32> {
        None
    }
    fn complete(_line: u32) {}
    fn register_external_handler(_handler: extern "C" fn()) {}
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
