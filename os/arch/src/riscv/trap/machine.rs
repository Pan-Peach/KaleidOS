//! M-mode trap 入口（`mtvec`/`mcause`/`mepc`/`mtval`）。
//!
//! 用于 RV32 M-mode / NoMMU bare-metal profile（无 OpenSBI 委托）。
//! 与 `supervisor` 共享本目录 `mod.rs` 的 `Trap`/`TrapFrame`/`Scause` 解码
//! （cause 编码在 S/M 模式一致）。
//!
use core::arch::global_asm;

use super::{Interrupt, Scause, Trap, TrapFrame};

#[cfg(target_arch = "riscv32")]
global_asm!(include_str!("machine32.S"));

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("machine64.S"));

pub fn init() {
    unsafe {
        set_trap_vector();
    }
}

unsafe fn set_trap_vector() {
    unsafe extern "C" {
        static trap_vec: u8;
    }

    let address = core::ptr::addr_of!(trap_vec) as usize;
    unsafe {
        core::arch::asm!(
            "csrw mtvec, {address}",
            address = in(reg) address,
            options(nostack, preserves_flags),
        );
    }
}

/// # Safety
///
/// 仅供汇编 trap 向量（`machine64.S` / `machine32.S`）调用：`trap_frame` 必须
/// 指向一个已完整保存、调用期间独占且保持有效的 `TrapFrame`（trap 栈上的现场），
/// 且 CPU 处于该现场对应的 trap 上下文。Rust 侧不得直接调用本入口。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trap_handler(trap_frame: *mut TrapFrame, raw_mcause: usize, mtval: usize) {
    let mcause = Scause::from_bits(raw_mcause);
    let trap = mcause.cause();
    let mepc = unsafe { (*trap_frame).epc };

    match trap {
        Trap::Interrupt(Interrupt::MachineTimer) => {
            super::dispatch_timer();
        }
        Trap::Interrupt(Interrupt::MachineExternal) => {
            super::dispatch_external();
        }
        Trap::Interrupt(_) => panic!(
            "unhandled interrupt: mcause={:#x}, mepc={:#x}, mtval={:#x}",
            raw_mcause, mepc, mtval
        ),
        Trap::Exception(_) => panic!(
            "unhandled exception: mcause={:#x}, mepc={:#x}, mtval={:#x}",
            raw_mcause, mepc, mtval
        ),
    }
}
