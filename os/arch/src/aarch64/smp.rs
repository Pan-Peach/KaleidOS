//! aarch64 SMP backend.
//!
//! 启动走 PSCI `CPU_ON`（SMC/HVC conduit）；IPI 走 GIC SGI（`ICC_SGI1R_EL1`）。
//! 参考 Linux `arch/arm64/kernel/smp.c` 的 `secondary_start_kernel` 流程。
//!
//! # 骨架约定
//!
//! `prepare` / `start_cpu` / `send_ipi*` 仍是 `todo!()`（PSCI AP bring-up 与
//! GICv3 SGI 投递是下一阶段）；Core 在 `kernel::init` 里只要求
//! `register_ipi_handler` 与 `init_cpu` 成功，且当前没有任何路径打开 IPI 源。

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
        todo!("aarch64 SMP: issue PSCI CPU_ON for the target MPIDR affinity")
    }

    fn init_ipi_cpu() -> Result<(), InitError> {
        // GICv3 SGI reception (`ICC_IGRPEN1_EL1` + redistributor PPI 27 wake)
        // belongs to the GIC bring-up and stays `todo!()`; no SGI is sent yet,
        // so there is no local source to unmask.  Core only needs `Ok`.
        Ok(())
    }

    fn register_ipi_handler(handler: LocalInterruptHandler) -> Result<(), InitError> {
        super::trap::register_ipi_handler(handler);
        Ok(())
    }

    fn enable_ipi_interrupt() {
        // No-op until GICv3 SGI reception exists (see `init_cpu`).  Core never
        // calls this during boot (`smp::init` deliberately leaves IPI masked).
    }

    fn send_ipi(_target: HardwareCpuId) -> Result<(), IpiError> {
        todo!("aarch64 SMP: send an SGI via ICC_SGI1R_EL1")
    }

    fn send_ipi_mask(_targets: &[HardwareCpuId]) -> Result<(), IpiError> {
        todo!("aarch64 SMP: send SGIs to a set of MPIDR affinities")
    }
}
