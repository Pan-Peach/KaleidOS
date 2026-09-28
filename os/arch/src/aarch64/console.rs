//! aarch64 early console transport (skeleton; bodies `todo!()`).
//!
//! Typical carrier: PL011 UART or a platform debug port.

/// Write one byte to the early console.
pub fn write_byte(_byte: u8) {
    todo!("aarch64: write one byte to the early console (e.g. PL011 UART)")
}

/// Read one byte from the early console, if any is available.
pub fn getc() -> Option<u8> {
    todo!("aarch64: read one byte from the early console")
}
