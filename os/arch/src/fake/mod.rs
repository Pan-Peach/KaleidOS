use crate::{Arch, ResetType};

pub struct Fake;

impl Arch for Fake {
    fn console_write_byte(byte: u8) {
        let _ = byte;
    }

    fn console_getc() -> Option<u8> {
        None
    }

    fn system_reset(_reset_type: ResetType) -> ! {
        loop {}
    }
}
