//! Minimal early console for RISC-V fatal paths.

use super::firmware;
use core::fmt::{self, Write};

pub fn write_byte(byte: u8) {
    firmware::console_putchar(byte);
}

pub fn write_fmt(args: fmt::Arguments<'_>) {
    let mut console = Console;
    let _ = console.write_fmt(args);
}

struct Console;

impl Write for Console {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for byte in value.bytes() {
            write_byte(byte);
        }
        Ok(())
    }
}
