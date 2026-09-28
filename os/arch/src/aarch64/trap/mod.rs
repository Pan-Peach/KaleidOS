//! aarch64 exception entry and dispatch (skeleton; bodies `todo!()`).
//!
//! Mirrors `riscv/trap/`: a vector table enters with an exception frame, then
//! Rust dispatch routes timer / external / IPI events to the Core-registered
//! handlers under the **logical** `CpuId`.

/// Minimal exception frame placeholder (skeleton).
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// Exception Syndrome Register value.
    pub esr: usize,
    /// Fault Address Register value.
    pub far: usize,
    /// Exception Link Register value.
    pub elr: usize,
}

/// Install the vector table and exception entry on the current CPU.
pub fn init() {
    todo!("aarch64: install the vector table and exception entry")
}

/// Store the Core timer handler.
pub fn register_timer_handler(_handler: crate::cpu::LocalInterruptHandler) {
    todo!("aarch64: store the timer handler")
}

/// Store the Core external-interrupt handler.
pub fn register_external_handler(_handler: crate::cpu::LocalInterruptHandler) {
    todo!("aarch64: store the external-interrupt handler")
}

/// Invoke the registered timer handler with the current logical `CpuId`.
pub fn dispatch_timer() {
    todo!("aarch64: dispatch the timer interrupt")
}

/// Invoke the registered external handler with the current logical `CpuId`.
pub fn dispatch_external() {
    todo!("aarch64: dispatch an external interrupt")
}
