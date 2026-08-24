use crate::{Arch, ResetType};
use sbi_rt;

pub struct Riscv64;

impl Arch for Riscv64 {
    fn console_write_byte(byte: u8) {
        sbi_rt::console_write_byte(byte);
    }

    fn console_getc() -> Option<u8> {
        let ch = sbi_rt::legacy::console_getchar();
        (ch != usize::MAX).then_some(ch as u8)
    }

    fn system_reset(reset_type: ResetType) -> ! {
        loop {
            // sbi_rt::Shutdown/ColdReboot/WarmReboot 是分别实现 ResetType trait 的
            // 不同 unit struct，无法 match 出统一类型 → 每个分支直接调用。
            let _ = match reset_type {
                ResetType::Shutdown => sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::NoReason),
                ResetType::ColdReboot => sbi_rt::system_reset(sbi_rt::ColdReboot, sbi_rt::NoReason),
                ResetType::WarmReboot => sbi_rt::system_reset(sbi_rt::WarmReboot, sbi_rt::NoReason),
            };
        }
    }
}
