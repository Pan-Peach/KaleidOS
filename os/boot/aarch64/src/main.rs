//! aarch64 boot skeleton (all bodies `todo!()`).
//!
//! Contract: discovery (device tree / ACPI) → `MachineInfo` → `kernel::init` →
//! Core Monitor or ArchTest.  None of that exists yet; this crate only pins the
//! entry shape so the profile cross-builds.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[cfg(feature = "vm-nommu")]
compile_error!("aarch64 boot requires `vm-mmu`");

#[cfg(feature = "selftest")]
#[path = "selftest.rs"]
mod selftest;

/// aarch64 启动入口（骨架）。
///
/// TODO: device-tree/ACPI discovery → `MachineInfo` → `kernel::init` → monitor /
/// selftest.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    #[cfg(feature = "selftest")]
    {
        selftest::run()
    }
    #[cfg(not(feature = "selftest"))]
    {
        todo!("aarch64 boot: discovery -> MachineInfo -> kernel::init -> Core Monitor")
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
