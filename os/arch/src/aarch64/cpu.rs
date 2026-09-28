//! aarch64 CPU backend（骨架；实现待手写）。
//!
//! per-CPU 基址用 `TPIDR_EL1`；runtime slot 用 `TPIDR_EL0`（二者分离）。
//! 所有方法体 `todo!()`。

use crate::cpu::{CpuId, LocalInterruptHandler};
use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};

/// aarch64 后端类型。
pub struct Aarch64;

/// aarch64 上下文记录（骨架；真实布局与 `switch` 汇编一致后钉死偏移）。
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aarch64Context {
    /// 程序计数器。
    pub pc: usize,
    /// 栈指针。
    pub sp: usize,
}

impl CpuArch for Aarch64 {
    type Context = Aarch64Context;
    type IrqFlags = usize;

    fn context_switch(_from: &mut Self::Context, _to: &Self::Context) {
        todo!("aarch64: task context switch")
    }

    fn new_context(_entry: usize, _stack_top: usize) -> Self::Context {
        todo!("aarch64: build a fresh task context")
    }

    fn init_cpu() {
        todo!("aarch64: architecture init (current CPU)")
    }

    fn enable_irq() {
        todo!("aarch64: enable global interrupts on this CPU")
    }

    fn runtime_slot() -> usize {
        todo!("aarch64: read the runtime slot (TPIDR_EL0; separate from per-CPU TPIDR_EL1)")
    }

    fn install_runtime_slot(_slot: usize) {
        todo!("aarch64: install the runtime slot")
    }

    fn set_context_slot(_context: &mut Self::Context, _slot: usize) {
        todo!("aarch64: store the runtime slot in a context record")
    }

    fn disable_irq() -> Self::IrqFlags {
        todo!("aarch64: mask interrupts and return prior flags (DAIF)")
    }

    fn restore_irq(_flags: Self::IrqFlags) {
        todo!("aarch64: restore interrupt flags")
    }

    fn wait_for_interrupt() {
        todo!("aarch64: wfi")
    }

    fn current_cpu() -> Option<CpuId> {
        todo!("aarch64: read the current CPU identity from the TPIDR_EL1 entry record")
    }

    fn per_cpu_base() -> Option<core::ptr::NonNull<()>> {
        todo!("aarch64: return the Core local storage pointer from the TPIDR_EL1 entry record")
    }

    unsafe fn install_per_cpu_base(_cpu: CpuId, _base: core::ptr::NonNull<()>) {
        todo!("aarch64: publish the Core local storage into the TPIDR_EL1 entry record")
    }
}

impl Timer for Aarch64 {
    fn init_cpu() -> Result<(), crate::smp::InitError> {
        todo!("new ISA: initialize this CPU's timer, disarmed and source-masked")
    }

    fn now() -> u64 {
        todo!("aarch64: monotonic time source (CNTVCT / CNTPCT)")
    }

    fn set_deadline(_deadline: u64) {
        todo!("aarch64: program this CPU's timer deadline")
    }

    fn cancel_deadline() {
        todo!("aarch64: cancel this CPU's timer deadline")
    }

    fn register_timer_handler(_handler: LocalInterruptHandler) {
        todo!("aarch64: register the timer interrupt handler")
    }

    fn enable_timer_interrupt() {
        todo!("aarch64: unmask this CPU's timer interrupt")
    }
}

impl InterruptController for Aarch64 {
    type Config = ();
    type Claim = ();

    unsafe fn configure(_config: ()) -> Result<(), crate::smp::InitError> {
        todo!("aarch64: configure the GIC distributor / redistributor")
    }

    fn init_cpu() -> Result<(), crate::smp::InitError> {
        todo!("aarch64: initialize this CPU's GIC interface, still masked")
    }

    fn enable(_line: u32) {
        todo!("aarch64: enable an external interrupt line")
    }

    fn disable(_line: u32) {
        todo!("aarch64: disable an external interrupt line")
    }

    fn claim() -> Option<Self::Claim> {
        todo!("aarch64: read the acknowledged INTID (GICC_IAR)")
    }

    fn claim_line(_claim: &Self::Claim) -> u32 {
        todo!("aarch64: map the acknowledged INTID to a line")
    }

    fn complete(_claim: Self::Claim) {
        todo!("aarch64: deactivate the interrupt (GICC_EOIR)")
    }

    fn register_external_handler(_handler: LocalInterruptHandler) {
        todo!("aarch64: register the external interrupt handler")
    }

    fn enable_external_interrupt() {
        todo!("aarch64: unmask external interrupts on this CPU")
    }
}

impl Console for Aarch64 {
    fn write_byte(byte: u8) {
        super::console::write_byte(byte)
    }

    fn getc() -> Option<u8> {
        super::console::getc()
    }
}

impl SystemReset for Aarch64 {
    fn system_reset(_reset_type: ResetType) -> ! {
        todo!("aarch64: system reset (PSCI SYSTEM_RESET)")
    }
}
