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

/// 安装 S-mode trap 向量（`stvec` = `trap_vec`）并装入安全 trap 栈约定：
/// 处理之外 `sscratch` 恒为 trap 栈顶，trap 入口据此在写任何东西之前换栈
/// （见 `trap32.S` / `trap64.S`）。
pub fn init() {
    unsafe {
        set_trap_vector();
        // rv64：`sscratch` 由 `install_per_cpu_base` 装成入口记录（见 cpu.rs），
        // 这里不能再写 trap 栈顶——那会覆盖记录指针。rv32：仍是旧约定
        // （`sscratch` = 全局 trap 栈顶）。
        #[cfg(target_arch = "riscv32")]
        set_scratch(super::trap_stack_top());
    }
}

/// 普通 Core trap 向量（`trap_vec`）的链接地址。
///
/// 普通 S-mode 执行（Core 或组件）的 `stvec` 恒为本地址：Isolated 的 trap 也
/// 走这条普通路径（Core 映射在每个实例 AS 里相同）。组装入口只需要在恢复
/// Core 现场时装回本地址。
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

/// 装入 `sscratch` 的 trap 栈约定值（仅 rv32：rv64 的 `sscratch` 由入口记录占用）。
#[cfg(target_arch = "riscv32")]
fn set_scratch(value: usize) {
    unsafe {
        core::arch::asm!("csrw sscratch, {value}",
            value = in(reg) value,
            options(nostack, preserves_flags),
        );
    }
}

/// 统一 trap 处理器（汇编入口调用）。
///
/// **返回语义**：`trap64.S`/`trap32.S` 保存完整 TrapFrame 后调用本函数；
/// **本函数正常返回**时，汇编侧恢复现场并 `sret` 回被打断的上下文
/// （返回路径已就绪）。异常与未知中断仍是 fatal（panic = 永不返回）。
///
/// `SupervisorTimer` / `SupervisorExternal` 分发给 Core 注册的 handler；
/// 未注册的 handler 是 Core 不变式破坏（handler 内显式 panic）。
///
/// # Safety
///
/// 仅供汇编 trap 向量（`trap64.S` / `trap32.S`）调用：`trap_frame` 必须指向
/// 一个已完整保存、调用期间独占且保持有效的 `TrapFrame`（trap 栈上的现场），
/// 且 CPU 处于该现场对应的 trap 上下文。Rust 侧不得直接调用本入口。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trap_handler(trap_frame: *mut TrapFrame, raw_scause: usize, stval: usize) {
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
        Trap::Exception(_) => {
            // Core 的异常钩子（组件故障归属 / 恢复判决）优先；未注册或拒绝
            // 恢复 = fatal（未证明可恢复的异常绝不静默）。
            if super::dispatch_exception(trap_frame, raw_scause, stval) {
                return;
            }
            panic!(
                "unhandled exception: scause={:#x}, sepc={:#x}, stval={:#x}",
                raw_scause, sepc, stval
            )
        }
    }
}
