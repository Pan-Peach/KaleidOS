//! RISC-V CPU implementation.
//!
//! The public type is named for the ISA family.  The XLEN-specific assembly
//! remains in `context/switch64.S`, so adding RV32 does not require renaming
//! the family-level CPU contract again.

use super::{console, firmware, trap};
use crate::{Console, CpuArch, ResetType, SystemReset, Timer};

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
        // TODO(C5): csrr 保存 sstatus.SIE → 清 SIE → 返回旧 sstatus；irq-save 进入。
        todo!("C5: sstatus.SIE irq-save")
    }

    fn restore_irq(_flags: Self::IrqFlags) {
        // TODO(C5): 恢复 sstatus.SIE（仅当保存值为开时置位）；irq-save 退出。
        todo!("C5: sstatus.SIE irq-restore")
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
