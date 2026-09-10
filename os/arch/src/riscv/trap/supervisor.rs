//! S-mode trap 入口：`stvec`/`scause`/`sepc`/`stval`。
//!
//! 当前唯一实现（OpenSBI 把 M-mode 之外的异常委托到 S-mode，见
//! `MEDELEG`/`MIDELEG`）。M-mode 裸机 profile 走 `super::machine`。

use core::arch::global_asm;

use super::{Scause, Trap, TrapFrame};

#[cfg(target_arch = "riscv32")]
global_asm!(include_str!("trap32.S"));

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("trap64.S"));

/// 安装 S-mode trap 向量（`stvec` = `trap_vec`）。
pub fn init() {
    unsafe {
        set_trap_vector();
    }
}

unsafe fn set_trap_vector() {
    unsafe extern "C" {
        static trap_vec: u8;
    }

    let addr = core::ptr::addr_of!(trap_vec) as usize;

    unsafe {
        core::arch::asm!("csrw stvec, {addr}",
            addr = in(reg) addr,
            options(nostack, preserves_flags),
        );
    }
}

/// 统一 trap 处理器（汇编入口调用，永不返回；S-mode 的 fatal 报告路径）。
#[unsafe(no_mangle)]
pub extern "C" fn trap_handler(trap_frame: *mut TrapFrame, raw_scause: usize, stval: usize) -> ! {
    let scause = Scause::from_bits(raw_scause);
    let trap = scause.cause();
    let sepc = unsafe { (*trap_frame).sepc };

    match trap {
        Trap::Interrupt(_) => panic!(
            "unhandled interrupt: scause={:#x}, sepc={:#x}, stval={:#x}",
            raw_scause, sepc, stval
        ),
        Trap::Exception(_) => panic!(
            "unhandled exception: scause={:#x}, sepc={:#x}, stval={:#x}",
            raw_scause, sepc, stval
        ),
    }
}
