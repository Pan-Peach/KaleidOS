//! x86_64 backend（骨架；实现待人类手写）。
//!
//! # 参考（实现时）
//!
//! - **per-CPU 基址**：kernel GS base（`IA32_GS_BASE` MSR），与 FS base 等
//!   架构状态分离。`swapgs` 属于特权级入口机制，不是普通任务切换或每次中断都做。
//! - **启动**：INIT → INIT-deassert → SIPI(vector)，低内存 trampoline；
//!   参考 Linux `arch/x86/kernel/smpboot.c` 与 DragonOS `arch/x86_64/smp`。
//! - **IPI**：xAPIC ICR（或 x2APIC MSR）；门铃语义与 Core pending work 分离。
//! - **timer**：local APIC timer 或 HPET/TSC-deadline；本 CPU 本地编程。
//!
//! 纯编码（`encoding.rs`）可在 host `test` 下编译与单测；硬件代码按
//! `target_os = "none"` 门控。

/// Early console transport (compiles on host too; bodies `todo!()`).
pub mod console;
pub mod elf;
pub mod encoding;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub mod context;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub mod cpu;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub mod mmu;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub mod smp;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub mod trap;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub use cpu::X86_64;
#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub use smp::X86_64SmpConfig;
