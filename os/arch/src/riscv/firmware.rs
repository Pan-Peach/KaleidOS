//! RISC-V firmware boundary.
//!
//! SBI is a firmware ABI, not an ISA primitive.  The CPU implementation wires
//! these operations into the `Console` and `SystemReset` contracts, while the
//! concrete SBI calls stay isolated in this module.

use crate::ResetType;
use sbi_rt;

#[cfg(feature = "machine")]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "machine")]
static MACHINE_MTIMECMP_BASE: AtomicUsize = AtomicUsize::new(0);

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
#[cfg(target_arch = "riscv64")]
pub fn time() -> u64 {
    let time: u64;

    unsafe {
        core::arch::asm!(
            "rdtime {time}",
            time = out(reg) time,
            options(nostack, preserves_flags),
        );
    }

    time
}

#[cfg(target_arch = "riscv32")]
pub fn time() -> u64 {
    let mut high: u32;
    let mut low: u32;
    let mut high_again: u32;

    unsafe {
        core::arch::asm!(
            "rdtime {high}",
            high = out(reg) high,
            options(nostack, preserves_flags),
        );

        core::arch::asm!(
            "rdtime {low}",
            low = out(reg) low,
            options(nostack, preserves_flags),
        );

        core::arch::asm!(
            "rdtimeh {high_again}",
            high_again = out(reg) high_again,
            options(nostack, preserves_flags),
        );
    }

    if high == high_again {
        return ((high as u64) << 32) | (low as u64);
    }
    panic!("Inconsistent time values");
}

/// 编程下一次时钟中断的绝对 deadline（SBI TIME 扩展）。
///
/// 到点后硬件置 `mip.STIP` → OpenSBI 委托为 `sip.STIP` → 在 `sie.STIE`
/// 打开时向 S-mode 投递时钟中断。`deadline` 与 [`time`] 同一基准。
///
#[cfg(feature = "supervisor")]
pub fn set_timer(deadline: u64) {
    sbi_rt::set_timer(deadline);
}

#[cfg(feature = "supervisor")]
pub fn enable_timer_interrupt() {
    unsafe {
        core::arch::asm!(
            "csrs sie, {mask}",
            mask = in(reg) (1usize << 5),
            options(nostack, preserves_flags),
        );
        core::arch::asm!(
            "csrs sstatus, {mask}",
            mask = in(reg) (1usize << 1),
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(feature = "machine")]
pub fn configure_machine_timer(mtimecmp_base: usize) {
    assert!(mtimecmp_base != 0, "machine timer has no mtimecmp address");
    MACHINE_MTIMECMP_BASE.store(mtimecmp_base, Ordering::Release);
}

#[cfg(feature = "machine")]
pub fn set_timer(deadline: u64) {
    let hart_id: usize;
    unsafe {
        core::arch::asm!(
            "csrr {hart_id}, mhartid",
            hart_id = out(reg) hart_id,
            options(nostack, preserves_flags),
        );
    }

    let mtimecmp_base = MACHINE_MTIMECMP_BASE.load(Ordering::Acquire);
    assert!(mtimecmp_base != 0, "machine timer was not configured");
    let address = mtimecmp_base + hart_id * 8;

    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::ptr::write_volatile(address as *mut u64, deadline);
    }

    #[cfg(target_arch = "riscv32")]
    unsafe {
        let low = address as *mut u32;
        let high = (address + 4) as *mut u32;

        // Prevent a transient low mtimecmp value while updating the 64-bit
        // register through two 32-bit MMIO accesses.
        core::ptr::write_volatile(high, u32::MAX);
        core::ptr::write_volatile(low, deadline as u32);
        core::ptr::write_volatile(high, (deadline >> 32) as u32);
    }
}

#[cfg(feature = "machine")]
pub fn enable_timer_interrupt() {
    unsafe {
        core::arch::asm!(
            "csrs mie, {mask}",
            mask = in(reg) (1usize << 7),
            options(nostack, preserves_flags),
        );
        core::arch::asm!(
            "csrs mstatus, {mask}",
            mask = in(reg) (1usize << 3),
            options(nostack, preserves_flags),
        );
    }
}
