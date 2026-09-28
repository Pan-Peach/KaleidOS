//! loongarch64 early console transport (skeleton; bodies `todo!()`).
//!
//! Typical carrier: 16550-compatible UART.

/// Write one byte to the early console.
pub fn write_byte(_byte: u8) {
    todo!("loongarch64: write one byte to the early console")
}

/// Read one byte from the early console, if any is available.
pub fn getc() -> Option<u8> {
    todo!("loongarch64: read one byte from the early console")
}
