//! loongarch64 backend（骨架；实现待人类手写）。
//!
//! # 参考（实现时）
//!
//! - **CSR**：CRMD/PRMD/ECFG/ESTAT/ERA/CPUID/TCFG/TVAL/TINTCLR/KSAVE*，见
//!   `encoding.rs`。`loongArch64` crate、Linux `arch/loongarch`、DragonOS
//!   `kernel/src/arch/loongarch64` 是主要对照。
//! - **per-CPU 基址**：显式保留的一个 `CSR.KSAVE` 槽（kernel per-CPU）；`$tp`
//!   仍留给组件 runtime slot。
//! - **启动**：IOCSR mailbox 交付 AP 入口地址 + IPI 唤醒（Linux/DragonOS 同款）。
//! - **IPI**：IOCSR IPI 寄存器组（status/en/set/clear）。
//! - **timer**：`TCFG`/`TVAL` 常量 timer；清除用 `TINTCLR`。
//!
//! 纯编码（`encoding.rs`）可在 host `test` 下编译与单测；硬件代码按
//! `target_os = "none"` 门控。

/// Early console transport (compiles on host too; bodies `todo!()`).
pub mod console;
pub mod elf;
pub mod encoding;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub mod context;
#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub mod cpu;
#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub mod mmu;
#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub mod smp;
#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub mod trap;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub use cpu::Loongarch64;
#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub use smp::Loongarch64SmpConfig;
