//! x86_64 SMP backend（骨架；实现待手写）。
//!
//! 启动路径参考 Linux `arch/x86/kernel/smpboot.c` 与 DragonOS
//! `arch/x86_64/smp`：低内存 AP trampoline + INIT/INIT-deassert/SIPI。
//! IPI 走 xAPIC ICR（或 x2APIC MSR）。

use super::X86_64;
use crate::cpu::HardwareCpuId;
use crate::smp::{CpuStartError, InitError, IpiError, LocalInterruptHandler, SecondaryBoot, Smp};

/// x86_64 启动配置，**由 boot 填充**。
pub struct X86_64SmpConfig {
    /// BSP 的本地 APIC id。
    pub boot_apic_id: u32,
    /// 低位启动 trampoline 的物理地址（AP 以实模式从 `vector<<12` 进入）。
    pub trampoline_pa: usize,
    /// bootstrap 页表物理地址（AP 在 trampoline 里装载 CR3）。
    pub bootstrap_cr3: u64,
}

impl Smp for X86_64 {
    type BootConfig = X86_64SmpConfig;

    unsafe fn prepare(_config: &'static Self::BootConfig) -> Result<(), InitError> {
        todo!("x86_64 SMP: copy the AP trampoline to low memory and publish startup data")
    }

    unsafe fn start_cpu(
        _target: HardwareCpuId,
        _boot: &'static SecondaryBoot,
    ) -> Result<(), CpuStartError> {
        todo!("x86_64 SMP: INIT -> INIT-deassert -> SIPI -> SIPI to the target APIC id")
    }

    fn init_cpu() -> Result<(), InitError> {
        todo!("x86_64 SMP: enable this CPU's local APIC IPI reception, still masked")
    }

    fn register_ipi_handler(_handler: LocalInterruptHandler) -> Result<(), InitError> {
        todo!("x86_64 SMP: register the IPI vector handler exactly once")
    }

    fn enable_ipi_interrupt() {
        todo!("x86_64 SMP: unmask this CPU's IPI vector")
    }

    fn send_ipi(_target: HardwareCpuId) -> Result<(), IpiError> {
        todo!("x86_64 SMP: send a doorbell IPI to one APIC id")
    }

    fn send_ipi_mask(_targets: &[HardwareCpuId]) -> Result<(), IpiError> {
        todo!("x86_64 SMP: send doorbell IPIs to a set of APIC ids")
    }
}
