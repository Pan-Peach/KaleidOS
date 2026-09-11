use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};
use core::sync::atomic::{AtomicBool, Ordering};

// 本 crate 整体 no_std；fake 仅在 host 编译（cfg 非 RV32/RV64），显式引入 std 供 console 直通。
extern crate std;
use std::{io::Write, println};

pub mod store;

/// Host 上的 trap 模拟：记录架构初始化是否已经安装了 trap 入口。
pub mod trap {
    use super::{AtomicBool, Ordering};

    static INITIALIZED: AtomicBool = AtomicBool::new(false);

    pub fn init() {
        INITIALIZED.store(true, Ordering::SeqCst);
    }

    pub fn is_initialized() -> bool {
        INITIALIZED.load(Ordering::SeqCst)
    }
}

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

    fn init() {
        trap::init();
    }

    fn disable_irq() -> Self::IrqFlags {
        // host 无真实中断：no-op 占位（irq-save 语义在真机上由 Riscv 实现）。
        0
    }

    fn restore_irq(_flags: Self::IrqFlags) {
        // host 无真实中断：no-op 占位。
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
    fn configure(_base: usize) {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_context_sets_entry_sp_and_clears_rest() {
        let ctx = Fake::new_context(0x8000_0000, 0x9000_0000);
        assert_eq!(ctx.pc(), 0x8000_0000, "pc = entry (resume point)");
        assert_eq!(ctx.reg(2), 0x9000_0000, "regs[2] = sp (stack top)");
        // 其余寄存器必须清零（ABI 首启未定义，清零安全）
        for i in 0..32 {
            if i != 2 {
                assert_eq!(ctx.reg(i), 0, "regs[{i}] should be zero");
            }
        }
    }

    #[test]
    fn init_installs_fake_trap() {
        assert!(!trap::is_initialized());
        Fake::init();
        assert!(trap::is_initialized());
    }
}
