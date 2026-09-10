//! RISC-V firmware boundary.
//!
//! SBI is a firmware ABI, not an ISA primitive.  The CPU implementation wires
//! these operations into the `Console` and `SystemReset` contracts, while the
//! concrete SBI calls stay isolated in this module.

use crate::ResetType;
use sbi_rt;

/// 刻意保留 legacy：当前 QEMU 的旧 OpenSBI (0.8) 未暴露 DBCN 扩展，
/// legacy console 是可移植的兜底。
#[allow(deprecated)]
pub fn console_putchar(byte: u8) {
    let _ = sbi_rt::legacy::console_putchar(byte as usize);
}

/// 刻意保留 legacy：当前 QEMU 的旧 OpenSBI (0.8) 未暴露 DBCN 扩展，
/// legacy console 是可移植的兜底。
#[allow(deprecated)]
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
        // 旧 OpenSBI 无 SRST → fallback 到 legacy shutdown。
        #[allow(deprecated)]
        sbi_rt::legacy::shutdown();
    }
    match ret.into_result() {
        Ok(value) => panic!("SBI system reset returned unexpectedly: value={:#x}", value),
        Err(error) => panic!("SBI system reset failed: {:?}", error),
    }
}

/// 读取当前时间（timebase tick，单调递增）。
///
/// TODO(C5)：实现。两条路（择一/带兜底）：
/// - S-mode 直接 `rdtime`（当前 QEMU/OpenSBI 已开 `scounteren`，boot 日志可见）；
/// - SBI TIME 扩展探测（更可移植，但多一次 ecall）。
pub fn time() -> u64 {
    todo!("C5: read time (rdtime / SBI TIME)")
}

/// 编程下一次时钟中断的绝对 deadline（SBI TIME 扩展）。
///
/// 到点后硬件置 `mip.STIP` → OpenSBI 委托为 `sip.STIP` → 在 `sie.STIE`
/// 打开时向 S-mode 投递时钟中断。`deadline` 与 [`time`] 同一基准。
///
/// TODO(C5)：实现（`sbi_rt::set_timer(deadline)`；SBI TIME 自 0.2 起可用，
/// 当前 QEMU 的 OpenSBI 0.8 兼容；无 sstc，走 SBI 是唯一路径）。
pub fn set_timer(_deadline: u64) {
    todo!("C5: SBI TIME set_timer")
}
