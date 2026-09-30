//! x86_64 ArchTest entry.
//!
//! Contract shared with `tests/qemu/arch_runner.py` (same as the RISC-V
//! harness): after full initialization print exactly `[selftest] ready`, read
//! one case name from the serial console, and on success print
//! `[selftest] <name>: PASS` then shut the machine down.  A failure prints
//! `[selftest] FAIL: <reason>` and panics (the runner treats that as a case
//! failure and kills QEMU).
//!
//! # Scope
//!
//! `boot` is the smoke case for this HAL bring-up: reaching the harness at all
//! proves entry -> long mode -> discovery -> `MachineInfo` -> `kernel::init` ->
//! console input all work.  The three hardware cases stay explicit `todo!()`
//! until their mechanisms land (they must fail loudly, not fake a PASS).

use arch::{ResetImpl, ResetType, SystemReset};
use kernel::machine::MachineInfo;

/// Enter the ArchTest harness.  Called from `bootstrap_main` under the
/// `selftest` feature, after `kernel::init` and `CpuArch::enable_irq`.
pub fn run(info: &MachineInfo) -> ! {
    kernel::log!("selftest", "ready");
    let mut command = [0u8; 64];
    let length = kernel::print::read_line(&mut command);
    kernel::printk!("\n");
    match &command[..length] {
        b"boot" => {
            kernel::log!(
                "selftest",
                "boot: cpus={} mem_regions={} devices={}",
                info.cpu_count,
                info.mem_count,
                info.dev_count
            );
            pass("boot")
        }
        b"smp-boot" => {
            todo!("x86_64 archtest: AP bring-up not implemented (SMP is an explicit todo!() boundary)")
        }
        b"timer" => {
            todo!("x86_64 archtest: one-shot deadline timer not implemented (no deadline source; the PIT is deliberately not started)")
        }
        b"external-irq" => {
            todo!("x86_64 archtest: APIC/IOAPIC claim/complete not implemented")
        }
        _ => fail("unknown command"),
    }
}

/// Report a passing case and power the machine off (observable QEMU exit).
fn pass(name: &str) -> ! {
    kernel::log!("selftest", "{}: PASS", name);
    ResetImpl::system_reset(ResetType::Shutdown)
}

/// Report a failing case; the panic also makes the serial log self-contained.
fn fail(reason: &str) -> ! {
    kernel::log!("selftest", "FAIL: {}", reason);
    panic!("selftest failed: {}", reason)
}
