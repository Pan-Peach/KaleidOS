//! RISC-V ISA family.
//!
//! XLEN-specific entry points live in their narrow variant modules/files;
//! address translation is selected below the ISA in `mmu/`.
//!
//! 每个子模块独立门控：asm/ISA 部分只在真实 RISC-V 目标编译；
//! 纯算法部分（`elf` 重定位、`mmu` 页表编码）在 host 的 test profile 下
//! 也编译，让 host 测试直接驱动生产实现（docs/testing.md §3/§5）。

#[cfg(target_arch = "riscv64")]
pub mod boot_vm;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod console;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod context;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod cpu;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod firmware;

/// 重定位实现：纯字节/编码逻辑，任何目标可编译（host 也测它本身）。
pub mod elf;

/// 地址翻译 backend：纯逻辑 + identity 指针解引用。
/// host 上仅在 cfg(test) 的 64 位目标编译（sv39/sv32 host 测试）。
#[cfg(any(
    target_arch = "riscv32",
    target_arch = "riscv64",
    all(test, target_pointer_width = "64")
))]
pub mod mmu;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod trap;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub use cpu::{Riscv, RiscvContext};