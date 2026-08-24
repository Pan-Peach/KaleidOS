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

#[cfg(not(target_arch = "riscv64"))]
pub mod fake;

#[cfg(target_arch = "riscv64")]
pub mod riscv64;

pub enum ResetType {
    Shutdown,
    ColdReboot,
    WarmReboot,
}

/// Arch 接口：ISA 无关的统一操作面（静态方法，无实例）。
/// host 编译用 FakeArch 实现，riscv64 用 Riscv64 实现（cfg 选择）。
pub trait Arch {
    fn console_write_byte(byte: u8);
    fn console_getc() -> Option<u8>;
    fn system_reset(reset_type: ResetType) -> !;
}

/// 当前平台的 Arch 实现（编译期确定：host → FakeArch，riscv64 → Riscv64）。
/// 使用处统一写 `arch::ArchImpl::xxx(...)` 或 `use arch::ArchImpl;`。
#[cfg(target_arch = "riscv64")]
pub type ArchImpl = riscv64::Riscv64;

#[cfg(not(target_arch = "riscv64"))]
pub type ArchImpl = fake::Fake;
