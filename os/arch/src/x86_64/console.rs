//! x86_64 early console transport (skeleton; bodies `todo!()`).
//!
//! Console is a boot/firmware transport capability, not an ISA primitive
//! (same split as `riscv/console.rs`).  Typical carriers: 16550 UART, early
//! console, or a platform-specific debug port.

/// Write one byte to the early console.
pub fn write_byte(_byte: u8) {
    todo!("x86_64: write one byte to the early console")
}

/// Read one byte from the early console, if any is available.
pub fn getc() -> Option<u8> {
    todo!("x86_64: read one byte from the early console")
}
