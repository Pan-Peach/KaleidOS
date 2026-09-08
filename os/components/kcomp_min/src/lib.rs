#![no_std]

unsafe extern "C" {
    #[link_name = "kcore_console_write_byte"]
    fn console_write_byte(byte: u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn kcomp_init() -> i32 {
    unsafe { console_write_byte(b'!') };
    0
}
