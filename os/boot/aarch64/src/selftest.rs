//! aarch64 ArchTest entry.
//!
//! Contract shared with `tests/qemu/arch_runner.py`:
//!   1. after full init, print exactly `[selftest] ready`;
//!   2. read one case name from the serial console;
//!   3. dispatch; on success print `[selftest] <name>: PASS` then reset, on
//!      failure print `[selftest] FAIL: <reason>` then panic.
//!
//! Case inventory (bring-up):
//!   - `boot`         — discovery → `kernel::init` → ready → PASS → shutdown;
//!   - `smp-boot`     — `todo!()`: PSCI AP bring-up (`Smp::prepare/start_cpu`);
//!   - `timer`        — `todo!()`: timer IRQ needs the GICv3 PPI route;
//!   - `external-irq` — `todo!()`: needs GICv3 claim/complete.
//!
//! Fault cases do not print PASS: they execute a faulting instruction and the
//! vector table's default handler reports ESR/FAR/ELR before panicking.

use arch::{ResetType, SystemReset};
use kernel::machine::MachineInfo;

/// Enter the ArchTest loop.  Called from `_start` under the `selftest` feature.
pub fn run(_info: &MachineInfo) -> ! {
    kernel::log!("selftest", "ready");
    let mut command = [0u8; 64];
    let length = kernel::print::read_line(&mut command);
    kernel::printk!("\n");
    match &command[..length] {
        b"boot" => boot(),
        b"smp-boot" => todo!("aarch64 archtest: PSCI CPU_ON AP bring-up (Smp::prepare/start_cpu)"),
        b"timer" => todo!("aarch64 archtest: timer IRQ needs GICv3 PPI 27 routing"),
        b"external-irq" => {
            todo!("aarch64 archtest: external IRQ needs GICv3 claim/complete")
        }
        _ => fail("unknown case"),
    }
}

/// `boot`: the harness itself proves the whole boot chain; the machine info was
/// already validated by `kernel::init` before `run` was entered.
fn boot() -> ! {
    pass("boot")
}

/// Success: report, then leave via PSCI `SYSTEM_OFF` (QEMU exits).
fn pass(name: &str) -> ! {
    kernel::log!("selftest", "{}: PASS", name);
    arch::ResetImpl::system_reset(ResetType::Shutdown)
}

/// Failure: report, then panic (the panic handler prints and parks the CPU).
fn fail(reason: &str) -> ! {
    kernel::log!("selftest", "FAIL: {}", reason);
    panic!("selftest failed: {}", reason)
}
