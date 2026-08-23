use crate::sbi;

pub fn putchar(c: u8) {
    sbi::dbcn_write_byte(c);
}

pub fn puts(s: &str) {
    for &byte in s.as_bytes() {
        putchar(byte);
    }
}

/// 打印 0x 前缀的最小 hex（无多余前导零）
pub fn put_hex(value: u64) {
    putchar(b'0');
    putchar(b'x');
    let mut started = false;
    for i in (0..16).rev() {
        let nibble = ((value >> (i * 4)) & 0xf) as u8;
        if nibble != 0 || started || i == 0 {
            started = true;
            let c = if nibble < 10 { b'0' + nibble } else { b'a' + (nibble - 10) };
            putchar(c);
        }
    }
}
