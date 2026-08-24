//! RISC-V 64 架构层（ISA）—— 顶层目录，与 kernel/ 平级。
//!
//! 组织方式参照 Linux `arch/<isa>/` 与 seL4 `src/arch/{arm,riscv,x86}`：
//! 每个 ISA 一个独立单元，内部只放指令集相关代码：
//! trap 入口与分发、寄存器上下文与 context switch、MMU/TLB、
//! 用户态模式切换、中断开关、原子/CPU 原语。
//!
//! M0 目标：启动入口（`_start`，接收 a0=hartid、a1=dtb 指针）与最小实现。
//! 后续架构：x86_64 / aarch64 / loongarch64（各自独立 crate）。

#![no_std]

#[cfg(test)]
extern crate std;

pub mod sbi;