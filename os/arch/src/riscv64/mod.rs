use crate::{Console, CpuArch, ResetType, SystemReset};
use core::arch::global_asm;

pub mod address_space;
pub mod boot_vm;
pub mod console;
pub mod firmware;
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

impl CpuArch for Riscv64 {
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

    fn init() {
        trap::init();
    }
}

impl Console for Riscv64 {
    fn write_byte(byte: u8) {
        console::write_byte(byte);
    }

    fn getc() -> Option<u8> {
        firmware::console_getc()
    }
}

impl SystemReset for Riscv64 {
    fn system_reset(reset_type: ResetType) -> ! {
        firmware::system_reset(reset_type)
    }
}
