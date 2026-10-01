//! RISC-V ISA family.
//!
//! XLEN-specific entry points live in their narrow variant modules/files;
//! address translation is selected below the ISA in `mmu/`.
//!
//! 每个子模块独立门控：asm/ISA 部分只在真实 RISC-V 目标编译；
//! 纯算法部分（`elf` 重定位、`mmu` 页表编码）在 host 的 test profile 下
//! 也编译，让 host 测试直接驱动生产实现（docs/development/testing.md §3/§5）。
//!
//! boot 期的内核页表策略（identity + high-half 双映射、段权限、临时 root）
//! 已移至 boot crate 的 `vm::{layout, bootstrap, runtime}`（boot policy 不
//! 属于 arch）；本层只保留 Sv39/Sv32 翻译机制（`mmu`）。

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod console;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod context;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod cpu;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod firmware;
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod plic;
/// RISC-V IPI backend；通用 CPU 启动描述符仍待接线。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod smp;

/// 私有 AS 的最小跨 AS 执行原语（同步进入 / 返回 / 放弃）：S-mode + MMU +
/// supervisor 才有意义；NoMMU / M-mode 构建不提供，也不静默降级。
#[cfg(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
pub mod trampoline;

/// 重定位实现：纯字节/编码逻辑，任何目标可编译（host 也测它本身）。
pub mod elf;

/// 地址翻译 backend：纯逻辑 + identity 指针解引用。
/// host 上仅在 cfg(test) 的 64 位目标编译（sv39/sv32 host 测试）。
#[cfg(all(
    feature = "vm-mmu",
    any(
        target_arch = "riscv32",
        target_arch = "riscv64",
        all(test, target_pointer_width = "64")
    )
))]
pub mod mmu;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub mod trap;

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub use cpu::{Riscv, RiscvContext};
