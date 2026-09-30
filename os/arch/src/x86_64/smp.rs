//! x86_64 SMP backend（骨架；实现待手写）。
//!
//! 启动路径参考 Linux `arch/x86/kernel/smpboot.c` 与 DragonOS
//! `arch/x86_64/smp`：低内存 AP trampoline + INIT/INIT-deassert/SIPI。
//! IPI 走 xAPIC ICR（或 x2APIC MSR）；编码在 [`super::encoding`]。
//!
//! # 本 bring-up 的范围
//!
//! BSP 单核启动路径只需要「注册回调 / 本地初始化 / 不解源」这三件事，
//! 因此它们已实现（APIC 本身未 bring-up，IPI 源保持 masked）。
//! 物理启动次 CPU（trampoline 复制、INIT/SIPI、ICR 投递）是明确的
//! `todo!()` 边界——`MachineInfo` 只报告 BSP，`core::init` 也不会调用它们。

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
        // Local APIC bring-up (SVR enable, spurious vector) is part of the AP
        // path; with no APIC yet, the IPI vector is trivially masked.
        Ok(())
    }

    fn register_ipi_handler(handler: LocalInterruptHandler) -> Result<(), InitError> {
        super::trap::register_ipi_handler(handler);
        Ok(())
    }

    fn enable_ipi_interrupt() {
        // APIC/IOAPIC delivery is not brought up; the vector stays masked.
    }

    fn send_ipi(_target: HardwareCpuId) -> Result<(), IpiError> {
        todo!("x86_64 SMP: send a doorbell IPI over the xAPIC ICR")
    }

    fn send_ipi_mask(_targets: &[HardwareCpuId]) -> Result<(), IpiError> {
        todo!("x86_64 SMP: send doorbell IPIs to a set of APIC ids")
    }
}
