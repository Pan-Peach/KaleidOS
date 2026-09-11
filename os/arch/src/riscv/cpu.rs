//! RISC-V CPU implementation.
//!
//! The public type is named for the ISA family.  The XLEN-specific assembly
//! remains in `context/switch64.S`, so adding RV32 does not require renaming
//! the family-level CPU contract again.

use super::{console, firmware, trap};
use crate::{Console, CpuArch, ResetType, SystemReset, Timer};
use core::arch::asm;

pub struct Riscv;

#[cfg(all(feature = "supervisor", not(feature = "machine")))]
const IRQ_ENABLE_BIT: usize = 1 << 1;

#[cfg(all(feature = "machine", not(feature = "supervisor")))]
const IRQ_ENABLE_BIT: usize = 1 << 3;

#[cfg(all(feature = "machine", feature = "supervisor"))]
const IRQ_ENABLE_BIT: usize = 0;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiscvContext {
    ra: usize,
    sp: usize,
    s: [usize; 12], // s0-s11
}

impl CpuArch for Riscv {
    type Context = RiscvContext;
    type IrqFlags = usize;

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

    fn disable_irq() -> Self::IrqFlags {
        let mut old: usize = 0;
        unsafe {
            #[cfg(all(feature = "supervisor", not(feature = "machine")))]
            asm!(
                "csrrc {old}, sstatus, {sie}",
                old = out(reg) old,
                sie = const IRQ_ENABLE_BIT,
            );
            #[cfg(all(feature = "machine", not(feature = "supervisor")))]
            asm!(
                "csrrc {old}, mstatus, {mie}",
                old = out(reg) old,
                mie = const IRQ_ENABLE_BIT,
            );
        }
        old
    }

    fn restore_irq(flags: Self::IrqFlags) {
        if flags & IRQ_ENABLE_BIT != 0 {
            unsafe {
                #[cfg(all(feature = "supervisor", not(feature = "machine")))]
                asm!(
                    "csrs sstatus, {mask}",
                    mask = in(reg) IRQ_ENABLE_BIT,
                );
                #[cfg(all(feature = "machine", not(feature = "supervisor")))]
                asm!(
                    "csrs mstatus, {mask}",
                    mask = in(reg) IRQ_ENABLE_BIT,
                );
            }
        }
    }
}

impl Timer for Riscv {
    fn now() -> u64 {
        // TODO(C5): 委托 `firmware::time()`（rdtime 或 SBI TIME）。
        todo!("C5: Timer::now")
    }

    fn set_deadline(_deadline: u64) {
        // TODO(C5): 委托 `firmware::set_timer(deadline)`（SBI TIME 扩展）。
        todo!("C5: Timer::set_deadline")
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
