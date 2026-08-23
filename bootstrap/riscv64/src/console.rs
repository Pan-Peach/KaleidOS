use crate::sbi;
use core::fmt::{self, Write};

struct Console;

impl Write for Console {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        puts(value);
        Ok(())
    }
}

pub fn putchar(c: u8) {
    sbi::dbcn_write_byte(c);
}

pub fn puts(s: &str) {
    for &byte in s.as_bytes() {
        putchar(byte);
    }
}

/// 带标签的前缀输出：[tag] msg（Linux dmesg 风格）
pub fn log(tag: &str, msg: &str) {
    puts("[");
    puts(tag);
    puts("] ");
    puts(msg);
}

pub fn print(args: fmt::Arguments<'_>) {
    let _ = Console.write_fmt(args);
}
