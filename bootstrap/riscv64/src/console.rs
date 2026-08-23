use crate::sbi;

pub fn putchar(c: u8) {
    sbi::dbcn_write_byte(c);
}

pub fn puts(s: &str) {
    for &byte in s.as_bytes() {
        putchar(byte);
    }
}
