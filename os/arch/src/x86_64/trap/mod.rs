//! x86_64 exception / interrupt entry and dispatch (skeleton; bodies `todo!()`).
//!
//! Mirrors `riscv/trap/`: entry assembly saves a frame, then Rust dispatch
//! routes timer / external / IPI events to the Core-registered handlers under
//! the **logical** `CpuId` (see [`crate::cpu::LocalInterruptHandler`]).

/// Minimal trap frame placeholder (skeleton).
///
/// The real layout must match the entry assembly byte-for-byte before offsets
/// are hard-coded; this placeholder exists only to pin the module shape.
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// Interrupt vector / exception number.
    pub vector: usize,
    /// Hardware error code where the architecture supplies one.
    pub error_code: usize,
}

/// Install the IDT and the trap entry path on the current CPU.
pub fn init() {
    todo!("x86_64: install the IDT and trap entry")
}

/// Store the Core timer handler.
pub fn register_timer_handler(_handler: crate::cpu::LocalInterruptHandler) {
    todo!("x86_64: store the timer handler")
}

/// Store the Core external-interrupt handler.
pub fn register_external_handler(_handler: crate::cpu::LocalInterruptHandler) {
    todo!("x86_64: store the external-interrupt handler")
}

/// Invoke the registered timer handler with the current logical `CpuId`.
pub fn dispatch_timer() {
    todo!("x86_64: dispatch the timer interrupt")
}

/// Invoke the registered external handler with the current logical `CpuId`.
pub fn dispatch_external() {
    todo!("x86_64: dispatch an external interrupt")
}
