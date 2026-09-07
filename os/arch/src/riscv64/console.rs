//! Minimal early console for RISC-V fatal paths.

use core::fmt::{self, Write};
use sbi_rt;

pub fn write_byte(byte: u8) {
    sbi_rt::console_write_byte(byte);
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
