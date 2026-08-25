use crate::{Arch, ResetType};
use sbi_rt;

pub struct Riscv64;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Riscv64Context {
    x: [usize; 32],
    sstate: usize,
    sepc: usize,
}

impl Arch for Riscv64 {
    type Context = Riscv64Context;

    fn context_switch(from: &mut Self::Context, to: &Self::Context) {}

    fn new_context(entry: usize, arg: usize, stack_top: usize) -> Self::Context {
        let mut ctx = Riscv64Context {
            x: [0; 32],
            sstate: 0,
            sepc: entry,
        };
        ctx.x[10] = arg; // a0
        ctx.x[2] = stack_top; // sp
        ctx
    }

    fn console_write_byte(byte: u8) {
        sbi_rt::console_write_byte(byte);
    }

    fn console_getc() -> Option<u8> {
        let ch = sbi_rt::legacy::console_getchar();
        (ch != usize::MAX).then_some(ch as u8)
    }

    fn system_reset(reset_type: ResetType) -> ! {
        loop {
            // sbi_rt::Shutdown/ColdReboot/WarmReboot 是分别实现 ResetType trait 的
            // 不同 unit struct，无法 match 出统一类型 → 每个分支直接调用。
            let _ = match reset_type {
                ResetType::Shutdown => sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::NoReason),
                ResetType::ColdReboot => sbi_rt::system_reset(sbi_rt::ColdReboot, sbi_rt::NoReason),
                ResetType::WarmReboot => sbi_rt::system_reset(sbi_rt::WarmReboot, sbi_rt::NoReason),
            };
        }
    }
}
