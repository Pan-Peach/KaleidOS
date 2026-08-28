use crate::{Arch, ResetType};
use core::sync::atomic::{AtomicBool, Ordering};

// 本 crate 整体 no_std；fake 仅在 host 编译（cfg 非 riscv64），显式引入 std 供 console 直通。
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
/// - `console_write_byte`：逐字节写 stdout 并 flush——串口语义（无缓冲、保序），
///   让 `printk!`/`log!` 在 host 测试里真实可见（cargo test 会捕获到测试输出）。
/// - `console_getc`：恒返回 `None`。`core::print::read_line()` 会忙等轮询，
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

impl Arch for Fake {
    type Context = FakeContext;
    const ELF_MACHINE: u16 = 0xF3; // 暂时先用RISC-V
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

    fn console_write_byte(byte: u8) {
        let mut out = std::io::stdout();
        let _ = out.write_all(&[byte]);
        let _ = out.flush();
    }

    fn console_getc() -> Option<u8> {
        None
    }

    fn system_reset(_reset_type: ResetType) -> ! {
        loop {
            core::hint::spin_loop();
        }
    }

    fn init() {
        trap::init();
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
