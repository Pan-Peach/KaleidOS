//! loongarch64 CPU backend（骨架；实现待手写）。
//!
//! per-CPU 基址放在显式保留的一个 `CSR.KSAVE` 槽。所有方法体 `todo!()`。
//! 参考 DragonOS `arch/loongarch64` 与 Linux `arch/loongarch`。

use crate::cpu::{CpuId, ExternalIrqHandler, LocalInterruptHandler};
use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};

/// loongarch64 后端类型。
pub struct Loongarch64;

/// loongarch64 上下文记录（骨架；真实布局与 `switch` 汇编一致后钉死偏移）。
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loongarch64Context {
    /// 返回地址（`$ra`）。
    pub ra: usize,
    /// 栈指针（`$sp`）。
    pub sp: usize,
}

impl CpuArch for Loongarch64 {
    type Context = Loongarch64Context;
    type IrqFlags = usize;

    fn context_switch(_from: &mut Self::Context, _to: &Self::Context) {
        todo!("loongarch64: task context switch")
    }

    fn new_context(_entry: usize, _stack_top: usize) -> Self::Context {
        todo!("loongarch64: build a fresh task context")
    }

    fn init_cpu() {
        todo!("loongarch64: architecture init (current CPU)")
    }

    fn enable_irq() {
        todo!("loongarch64: enable global interrupts on this CPU")
    }

    fn disable_irq() -> Self::IrqFlags {
        todo!("loongarch64: disable interrupts via CRMD.IE and return prior flags")
    }

    fn restore_irq(_flags: Self::IrqFlags) {
        todo!("loongarch64: restore interrupt flags")
    }

    fn wait_for_interrupt() {
        todo!("loongarch64: idle (idle instruction)")
    }

    unsafe fn atomic_idle(_flags: Self::IrqFlags) {
        todo!("loongarch64: atomic check→idle (idle instruction, CRMD.IE restore)")
    }

    fn current_cpu() -> Option<CpuId> {
        todo!(
            "loongarch64: return the Core-assigned *logical* CpuId from the arch entry record (NOT CSR.CPUID)"
        )
    }

    fn per_cpu_base() -> Option<core::ptr::NonNull<()>> {
        todo!("loongarch64: return the Core local storage pointer from the reserved CSR.KSAVE slot")
    }

    unsafe fn install_per_cpu_base(_cpu: CpuId, _base: core::ptr::NonNull<()>) {
        todo!("loongarch64: publish the Core local storage into the reserved CSR.KSAVE slot")
    }
}

impl Timer for Loongarch64 {
    fn init_cpu() -> Result<(), crate::TimerError> {
        // No timer machinery exists on this skeleton; never claim readiness.
        Err(crate::TimerError::Unsupported)
    }

    fn now() -> u64 {
        todo!("loongarch64: monotonic time source (stable counter via RDTIME*, not CNTC)")
    }

    fn set_deadline(_deadline: u64) -> Result<(), crate::TimerError> {
        Err(crate::TimerError::Unsupported)
    }

    fn cancel_deadline() {
        todo!("loongarch64: cancel this CPU's timer deadline")
    }

    fn register_timer_handler(_handler: LocalInterruptHandler) {
        todo!("loongarch64: register the timer interrupt handler")
    }

    fn enable_timer_interrupt() -> Result<(), crate::TimerError> {
        // No route / CPU interface exists yet; report delivery honestly.
        Err(crate::TimerError::DeliveryUnavailable)
    }
}

impl InterruptController for Loongarch64 {
    type Config = ();

    unsafe fn configure(_config: ()) -> Result<(), crate::smp::InitError> {
        todo!("loongarch64: configure the interrupt controller / EIOINTC")
    }

    fn init_cpu() -> Result<(), crate::smp::InitError> {
        todo!("loongarch64: initialize this CPU's interrupt interface, still masked")
    }

    fn enable(_line: u32) {
        todo!("loongarch64: enable an external interrupt line")
    }

    fn disable(_line: u32) {
        todo!("loongarch64: disable an external interrupt line")
    }

    fn register_external_handler(_handler: ExternalIrqHandler) {
        todo!("loongarch64: register the external interrupt handler")
    }

    fn enable_external_interrupt() {
        todo!("loongarch64: unmask external interrupts on this CPU")
    }
}

impl Console for Loongarch64 {
    fn write_byte(byte: u8) {
        super::console::write_byte(byte)
    }

    fn getc() -> Option<u8> {
        super::console::getc()
    }
}

impl SystemReset for Loongarch64 {
    fn system_reset(_reset_type: ResetType) -> ! {
        todo!("loongarch64: system reset")
    }
}
