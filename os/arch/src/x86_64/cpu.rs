//! x86_64 CPU backend（骨架；实现待手写）。
//!
//! 所有方法体 `todo!()`；接口形状与本 crate 的 `riscv`/`fake` 后端一致。
//! per-CPU 基址用 kernel GS base（见 [`super::encoding::MSR_GS_BASE`]）。

use crate::cpu::{CpuId, LocalInterruptHandler};
use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};

/// x86_64 后端类型。
pub struct X86_64;

/// x86_64 上下文记录（骨架；真实布局与 `switch` 汇编一致后钉死偏移）。
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86_64Context {
    /// 指令指针。
    pub rip: usize,
    /// 栈指针。
    pub rsp: usize,
}

impl CpuArch for X86_64 {
    type Context = X86_64Context;
    type IrqFlags = usize;

    fn context_switch(_from: &mut Self::Context, _to: &Self::Context) {
        todo!("x86_64: task context switch")
    }

    fn new_context(_entry: usize, _stack_top: usize) -> Self::Context {
        todo!("x86_64: build a fresh task context")
    }

    fn init_cpu() {
        todo!("x86_64: architecture init (current CPU)")
    }

    fn enable_irq() {
        todo!("x86_64: enable global interrupts on this CPU")
    }

    fn runtime_slot() -> usize {
        todo!("x86_64: read the runtime slot (FS-base separation from GS per-CPU base)")
    }

    fn install_runtime_slot(_slot: usize) {
        todo!("x86_64: install the runtime slot")
    }

    fn set_context_slot(_context: &mut Self::Context, _slot: usize) {
        todo!("x86_64: store the runtime slot in a context record")
    }

    fn disable_irq() -> Self::IrqFlags {
        todo!("x86_64: disable interrupts and return prior flags")
    }

    fn restore_irq(_flags: Self::IrqFlags) {
        todo!("x86_64: restore interrupt flags")
    }

    fn wait_for_interrupt() {
        todo!("x86_64: hlt")
    }

    fn current_cpu() -> Option<CpuId> {
        todo!("x86_64: read the current CPU identity from the GS-base entry record")
    }

    fn per_cpu_base() -> Option<core::ptr::NonNull<()>> {
        todo!("x86_64: return the Core local storage pointer from the GS-base entry record")
    }

    unsafe fn install_per_cpu_base(_cpu: CpuId, _base: core::ptr::NonNull<()>) {
        todo!("x86_64: publish the Core local storage into the GS-base entry record")
    }
}

impl Timer for X86_64 {
    fn init_cpu() -> Result<(), crate::smp::InitError> {
        todo!("new ISA: initialize this CPU's timer, disarmed and source-masked")
    }

    fn now() -> u64 {
        todo!("x86_64: monotonic time source (TSC / APIC timer)")
    }

    fn set_deadline(_deadline: u64) {
        todo!("x86_64: program this CPU's timer deadline")
    }

    fn cancel_deadline() {
        todo!("x86_64: cancel this CPU's timer deadline")
    }

    fn register_timer_handler(_handler: LocalInterruptHandler) {
        todo!("x86_64: register the timer interrupt handler")
    }

    fn enable_timer_interrupt() {
        todo!("x86_64: unmask this CPU's timer interrupt")
    }
}

impl InterruptController for X86_64 {
    type Config = ();
    type Claim = ();

    unsafe fn configure(_config: ()) -> Result<(), crate::smp::InitError> {
        todo!("x86_64: configure the local APIC / IOAPIC")
    }

    fn init_cpu() -> Result<(), crate::smp::InitError> {
        todo!("x86_64: initialize this CPU's APIC interface, still masked")
    }

    fn enable(_line: u32) {
        todo!("x86_64: enable an external interrupt line")
    }

    fn disable(_line: u32) {
        todo!("x86_64: disable an external interrupt line")
    }

    fn claim() -> Option<Self::Claim> {
        todo!("x86_64: acknowledge the delivered vector (not PLIC-style polling)")
    }

    fn claim_line(_claim: &Self::Claim) -> u32 {
        todo!("x86_64: map the claimed vector to a line")
    }

    fn complete(_claim: Self::Claim) {
        todo!("x86_64: EOI the interrupt")
    }

    fn register_external_handler(_handler: LocalInterruptHandler) {
        todo!("x86_64: register the external interrupt handler")
    }

    fn enable_external_interrupt() {
        todo!("x86_64: unmask external interrupts on this CPU")
    }
}

impl Console for X86_64 {
    fn write_byte(byte: u8) {
        super::console::write_byte(byte)
    }

    fn getc() -> Option<u8> {
        super::console::getc()
    }
}

impl SystemReset for X86_64 {
    fn system_reset(_reset_type: ResetType) -> ! {
        todo!("x86_64: system reset (ACPI / keyboard controller / triple fault)")
    }
}
