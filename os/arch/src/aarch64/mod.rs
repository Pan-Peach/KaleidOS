//! aarch64 backend（骨架；实现待人类手写）。
//!
//! # 参考（实现时）
//!
//! - **per-CPU 基址**：`TPIDR_EL1`（特权态），与其它架构状态分离。
//! - **启动**：PSCI `CPU_ON`（SMC/HVC conduit）或平台释放机制；AP 从
//!   `secondary_entry` 汇编进入。参考 Linux `arch/arm64/kernel/smp.c`。
//! - **IPI**：GIC SGI（`ICC_SGI1R_EL1`）。
//! - **timer**：`CNTP_*` / `CNTV_*`，本 CPU 本地编程。
//!
//! 纯编码（`encoding.rs`）可在 host `test` 下编译与单测；硬件代码按
//! `target_os = "none"` 门控。

/// Early console transport (compiles on host too; bodies `todo!()`).
pub mod console;
pub mod elf;
pub mod encoding;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub mod context;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub mod cpu;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub mod mmu;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub mod smp;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub mod trap;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub use cpu::Aarch64;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub use smp::Aarch64SmpConfig;
