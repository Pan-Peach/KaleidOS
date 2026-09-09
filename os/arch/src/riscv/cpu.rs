//! RISC-V CPU implementation.
//!
//! The public type is named for the ISA family.  The XLEN-specific assembly
//! remains in `context/switch64.S`, so adding RV32 does not require renaming
//! the family-level CPU contract again.

use super::{console, firmware, trap};
use crate::{Console, CpuArch, ResetType, SystemReset};

pub struct Riscv;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiscvContext {
    ra: usize,
    sp: usize,
    s: [usize; 12], // s0-s11
}

impl CpuArch for Riscv {
    type Context = RiscvContext;

    fn context_switch(from: &mut Self::Context, to: &Self::Context) {
        unsafe extern "C" {
            fn __switch(from: *mut RiscvContext, to: *const RiscvContext);
        }
        unsafe {
            __switch(from as *mut RiscvContext, to as *const RiscvContext);
        }
    }

    fn new_context(entry: usize, stack_top: usize) -> Self::Context {
        RiscvContext {
            ra: entry,
            sp: stack_top,
            s: [0; 12],
        }
    }

    fn init() {
        trap::init();
    }
}

impl Console for Riscv {
    fn write_byte(byte: u8) {
        console::write_byte(byte);
    }

    fn getc() -> Option<u8> {
        firmware::console_getc()
    }
}

impl SystemReset for Riscv {
    fn system_reset(reset_type: ResetType) -> ! {
        firmware::system_reset(reset_type)
    }
}
