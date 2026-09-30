//! loongarch64 SMP backend（骨架；实现待手写）。
//!
//! 启动：IOCSR mailbox 交付 AP 入口地址，再 IPI 唤醒；AP 从 firmware 的
//! 启动入口进入。IPI 走 IOCSR IPI 寄存器组。参考 DragonOS
//! `arch/loongarch64` 与 Linux `arch/loongarch/kernel/smp.c`。

use super::Loongarch64;
use crate::cpu::HardwareCpuId;
use crate::smp::{CpuStartError, InitError, IpiError, LocalInterruptHandler, SecondaryBoot, Smp};

/// loongarch64 启动配置，**由 boot 填充**。
///
/// 只带硬件身份；具体的 mailbox / IPI 资源在实现选定固件协议后再补
/// （IOCSR 不是经 discovery 得到的 MMIO 基址）。
pub struct Loongarch64SmpConfig {
    /// BSP 的**硬件**身份（不是逻辑 `CpuId`）。
    pub boot_hardware_id: HardwareCpuId,
}

impl Smp for Loongarch64 {
    type BootConfig = Loongarch64SmpConfig;

    unsafe fn prepare(_config: &'static Self::BootConfig) -> Result<(), InitError> {
        todo!("loongarch64 SMP: publish the firmware/mailbox startup data for APs")
    }

    unsafe fn start_cpu(
        _target: HardwareCpuId,
        _boot: &'static SecondaryBoot,
    ) -> Result<(), CpuStartError> {
        todo!("loongarch64 SMP: write the AP entry address into the mailbox and IPI it")
    }

    fn init_ipi_cpu() -> Result<(), InitError> {
        todo!("loongarch64 SMP: enable this CPU's IOCSR IPI reception, still masked")
    }

    fn register_ipi_handler(_handler: LocalInterruptHandler) -> Result<(), InitError> {
        todo!("loongarch64 SMP: register the IPI handler exactly once")
    }

    fn enable_ipi_interrupt() {
        todo!("loongarch64 SMP: unmask this CPU's IPI source")
    }

    fn send_ipi(_target: HardwareCpuId) -> Result<(), IpiError> {
        todo!("loongarch64 SMP: set the target's IOCSR IPI bit")
    }

    fn send_ipi_mask(_targets: &[HardwareCpuId]) -> Result<(), IpiError> {
        todo!("loongarch64 SMP: set IOCSR IPI bits for a set of cores")
    }
}
