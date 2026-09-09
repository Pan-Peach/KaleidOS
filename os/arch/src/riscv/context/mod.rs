//! CPU context switching implementations, selected by XLEN.

use core::arch::global_asm;

#[cfg(target_arch = "riscv32")]
global_asm!(include_str!("switch32.S"));

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("switch64.S"));
