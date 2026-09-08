use crate::{Arch, ResetType};
use core::arch::global_asm;
use sbi_rt;

pub mod console;
pub mod mmu;
pub mod sv39;
pub mod trap;

global_asm!(include_str!("switch.S"));

pub struct Riscv64;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Riscv64Context {
    ra: usize,
    sp: usize,
    s: [usize; 12], // s0-s11
}

impl Arch for Riscv64 {
    type Context = Riscv64Context;
    const ELF_MACHINE: u16 = 0xF3;
    fn context_switch(from: &mut Self::Context, to: &Self::Context) {
        unsafe extern "C" {
            fn __switch(from: *mut Riscv64Context, to: *const Riscv64Context);
        }
        unsafe {
            __switch(from as *mut Riscv64Context, to as *const Riscv64Context);
        }
    }

    fn new_context(entry: usize, stack_top: usize) -> Self::Context {
        Riscv64Context {
            ra: entry,
            sp: stack_top,
            s: [0; 12],
        }
    }

    fn console_write_byte(byte: u8) {
        console::write_byte(byte);
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

    fn init() {
        trap::init();
    }
}
