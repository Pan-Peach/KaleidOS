//! RISC-V SMP backend（骨架；实现待手写）。
//!
//! # 参考（人类实现时）
//!
//! - **启动握手**：Linux `arch/loongarch`/`arch/riscv` 的 `smp_ops` 分裂
//!   （BSP `smp_prepare_cpus` / AP `__cpu_up`）；RISC-V 常见路径是 SBI HSM
//!   （`sbi_hart_start`）或 CLINT/固件 mailbox。OpenSBI legacy 控制台不支持
//!   HSM，则需 `-bios default` 的 SBI 版本或直接 mailbox。
//! - **IPI**：SBI IPI 扩展（`sbi_send_ipi`，软件中断 `SIP.SSIP`）或 CLINT
//!   `MSIP(hartid)`；本后端需把「先发布 pending、再响铃」的排序做对。
//! - **per-CPU 基址**：**不要**用 `tp`（Core 的 runtime slot）；正确载体是
//!   `sscratch` 升级成的 arch 私有入口记录（见 `context` / `trap` 的现有约定）。
//!
//! # 骨架约定
//!
//! 所有方法体为 `todo!()`；接口/错误类型/配置形状已定，留给人类实现。

use super::Riscv;
use super::trap;
use crate::cpu::HardwareCpuId;
use crate::smp::{CpuStartError, InitError, IpiError, LocalInterruptHandler, SecondaryBoot, Smp};
use core::arch::asm;

/// RISC-V 启动配置，**由 boot 填充**（不是通用 Core 能构造的）。
///
/// 骨架字段只表达「后端需要什么」，具体布局在实现时定稿。
pub struct RiscvSmpConfig {
    /// BSP 的硬件身份（S 模式读不到 `mhartid`，由 boot/固件 `a0` 告知）。
    pub boot_hardware_id: HardwareCpuId,
    /// 启动 AP 的信道基址（CLINT `mtimecmp`/`MSIP` 或 SBI 语义下的保留字段）。
    pub clint_base: usize,
}

impl Smp for Riscv {
    type BootConfig = RiscvSmpConfig;

    unsafe fn prepare(_config: &'static Self::BootConfig) -> Result<(), InitError> {
        todo!("SMP(riscv): prepare CLINT/SBI IPI resources and AP trampoline mappings")
    }

    unsafe fn start_cpu(
        _target: HardwareCpuId,
        _boot: &'static SecondaryBoot,
    ) -> Result<(), CpuStartError> {
        todo!("SMP(riscv): hand the AP its entry/stack via SBI HSM or mailbox, then IPI it")
    }

    fn init_cpu() -> Result<(), InitError> {
        unsafe { asm!("csrc sip, {}", in(reg) (1usize << 1), options(nostack)) };
        Ok(())
    }

    fn register_ipi_handler(handler: LocalInterruptHandler) -> Result<(), InitError> {
        trap::register_ipi_handler(handler);
        Ok(())
    }

    fn enable_ipi_interrupt() {
        unsafe { asm!("csrs sie, {}", in(reg) (1usize << 1), options(nostack)) };
    }

    fn send_ipi(target: HardwareCpuId) -> Result<(), IpiError> {
        // 发布 → 通知排序（Oracle 评审）：Core 已用 Release 发布 pending 位；在触发
        // 固件通知前再加一道全序 `fence`，确保该存储在门铃之前对目标 CPU 可见。
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let mask = sbi_rt::HartMask::from_mask_base(1, target.raw() as usize);
        if sbi_rt::send_ipi(mask).is_err() {
            return Err(IpiError::DeliveryFailed);
        }
        Ok(())
    }

    fn send_ipi_mask(targets: &[HardwareCpuId]) -> Result<(), IpiError> {
        for t in targets {
            Self::send_ipi(*t)?;
        }
        Ok(())
    }
}
