//! S-mode trap 入口：`stvec`/`scause`/`sepc`/`stval`。
//!
//! 当前唯一实现（OpenSBI 把 M-mode 之外的异常委托到 S-mode，见
//! `MEDELEG`/`MIDELEG`）。M-mode 裸机 profile 走 `super::machine`。

use core::arch::global_asm;

use super::{Interrupt, Scause, Trap, TrapFrame};

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

/// 普通 Core trap 向量（`trap_vec`）的链接地址。
///
/// gateway 在组件运行期间把 `stvec` 临时换成自己的双映射入口，切回 Core 前必须
/// 装回本地址，使嵌套 / 普通 Core trap 不走组件入口路径。
pub fn vector_address() -> usize {
    unsafe extern "C" {
        static trap_vec: u8;
    }
    core::ptr::addr_of!(trap_vec) as usize
}

unsafe fn set_trap_vector() {
    let addr = vector_address();

    unsafe {
        core::arch::asm!("csrw stvec, {addr}",
            addr = in(reg) addr,
            options(nostack, preserves_flags),
        );
    }
}

/// 统一 trap 处理器（汇编入口调用）。
///
/// **返回语义（C5 起）**：`trap64.S`/`trap32.S` 保存完整 TrapFrame 后调用本
/// 函数；**本函数正常返回**时，汇编侧恢复现场并 `sret` 回被打断的上下文
/// （返回路径已就绪）。异常与未知中断仍是 fatal（panic = 永不返回）。
///
/// 当前 `SupervisorTimer` 分支是 C5 的骨架位（`todo!()`）：在时钟中断真正
/// 开闸（`sie.STIE` + `sstatus.SIE`）之前不会被触达。
#[unsafe(no_mangle)]
pub extern "C" fn trap_handler(trap_frame: *mut TrapFrame, raw_scause: usize, stval: usize) {
    let scause = Scause::from_bits(raw_scause);
    let trap = scause.cause();
    let sepc = unsafe { (*trap_frame).epc };

    match trap {
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            super::dispatch_timer();
        }
        Trap::Interrupt(Interrupt::SupervisorExternal) => {
            super::dispatch_external();
        }
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
