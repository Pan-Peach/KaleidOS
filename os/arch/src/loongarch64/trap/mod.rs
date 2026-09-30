//! loongarch64 exception entry and dispatch (skeleton; bodies `todo!()`).
//!
//! Mirrors `riscv/trap/`: an exception entry reads ERA/ESTAT/BADV, then Rust
//! dispatch routes timer / external / IPI events to the Core-registered
//! handlers under the **logical** `CpuId`.

/// Minimal exception frame placeholder (skeleton).
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// Exception return address (CSR.ERA).
    pub era: usize,
    /// Exception status (CSR.ESTAT).
    pub estat: usize,
    /// Bad virtual address (CSR.BADV).
    pub badv: usize,
}

/// Install the exception entry (CSR.EENTRY/ECFG) on the current CPU.
pub fn init() {
    todo!("loongarch64: install the exception entry (EENTRY/ECFG)")
}

/// Store the Core timer handler.
pub fn register_timer_handler(_handler: crate::cpu::LocalInterruptHandler) {
    todo!("loongarch64: store the timer handler")
}

/// Store the Core external-interrupt callback `(logical CpuId, logical IRQ)`.
pub fn register_external_handler(_handler: crate::cpu::ExternalIrqHandler) {
    todo!("loongarch64: store the external-interrupt handler")
}

/// Invoke the registered timer handler with the current logical `CpuId`.
pub fn dispatch_timer() {
    todo!("loongarch64: dispatch the timer interrupt")
}

/// Invoke the registered external handler with the current logical `CpuId`.
pub fn dispatch_external() {
    todo!("loongarch64: dispatch an external interrupt")
}
