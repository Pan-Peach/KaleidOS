//! RISC-V firmware boundary.
//!
//! SBI is a firmware ABI, not an ISA primitive.  The CPU implementation wires
//! these operations into the `Console` and `SystemReset` contracts, while the
//! concrete SBI calls stay isolated in this module.

use crate::ResetType;
use sbi_rt;

pub fn console_putchar(byte: u8) {
    let _ = sbi_rt::legacy::console_putchar(byte as usize);
}

pub fn console_getc() -> Option<u8> {
    let ch = sbi_rt::legacy::console_getchar();
    (ch != usize::MAX).then_some(ch as u8)
}

pub fn system_reset(reset_type: ResetType) -> ! {
    // sbi_rt::Shutdown/ColdReboot/WarmReboot 是分别实现 ResetType trait 的
    // 不同 unit struct，无法 match 出统一类型 → 每个分支直接调用。
    let is_shutdown = matches!(&reset_type, ResetType::Shutdown);
    let ret = match reset_type {
        ResetType::Shutdown => sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::NoReason),
        ResetType::ColdReboot => sbi_rt::system_reset(sbi_rt::ColdReboot, sbi_rt::NoReason),
        ResetType::WarmReboot => sbi_rt::system_reset(sbi_rt::WarmReboot, sbi_rt::NoReason),
    };
    // OpenSBI 0.8 used by the current QEMU image may not expose the newer
    // SRST extension, while its legacy shutdown call is available.
    // Keep reboot failures explicit: there is no portable legacy reboot
    // equivalent in the SBI interface.
    if is_shutdown && ret == sbi_rt::SbiRet::not_supported() {
        sbi_rt::legacy::shutdown();
    }
    match ret.into_result() {
        Ok(value) => panic!("SBI system reset returned unexpectedly: value={:#x}", value),
        Err(error) => panic!("SBI system reset failed: {:?}", error),
    }
}
