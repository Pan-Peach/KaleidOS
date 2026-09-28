//! aarch64 SMP backend（骨架；实现待手写）。
//!
//! 启动走 PSCI `CPU_ON`（SMC/HVC conduit）；IPI 走 GIC SGI。
//! 参考 Linux `arch/arm64/kernel/smp.c` 的 `secondary_start_kernel` 流程。

use super::Aarch64;
use crate::cpu::HardwareCpuId;
use crate::smp::{CpuStartError, InitError, IpiError, LocalInterruptHandler, SecondaryBoot, Smp};

/// aarch64 启动配置，**由 boot 填充**。
pub struct Aarch64SmpConfig {
    /// PSCI conduit：false = SMC，true = HVC。
    pub conduit_hvc: bool,
    /// AP 入口的物理地址（PSCI `CPU_ON` 的 entry point）。
    pub entry_pa: u64,
}

impl Smp for Aarch64 {
    type BootConfig = Aarch64SmpConfig;

    unsafe fn prepare(_config: &'static Self::BootConfig) -> Result<(), InitError> {
        todo!("aarch64 SMP: validate the PSCI conduit and publish the AP entry/stack")
    }

    unsafe fn start_cpu(
        _target: HardwareCpuId,
        _boot: &'static SecondaryBoot,
    ) -> Result<(), CpuStartError> {
        todo!("aarch64 SMP: issue PSCI CPU_ON for the target MPIDR")
    }

    fn init_cpu() -> Result<(), InitError> {
        todo!("aarch64 SMP: enable this CPU's GIC SGI reception, still masked")
    }

    fn register_ipi_handler(_handler: LocalInterruptHandler) -> Result<(), InitError> {
        todo!("aarch64 SMP: register the SGI handler exactly once")
    }

    fn enable_ipi_interrupt() {
        todo!("aarch64 SMP: unmask this CPU's SGI source")
    }

    fn send_ipi(_target: HardwareCpuId) -> Result<(), IpiError> {
        todo!("aarch64 SMP: send an SGI to one MPIDR")
    }

    fn send_ipi_mask(_targets: &[HardwareCpuId]) -> Result<(), IpiError> {
        todo!("aarch64 SMP: send SGIs to a set of MPIDRs")
    }
}
