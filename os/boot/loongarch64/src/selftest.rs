//! loongarch64 ArchTest entry (skeleton; bodies `todo!()`).
//!
//! Contract shared with `tests/qemu/arch_runner.py`:
//!   1. after full init, print exactly `[selftest] ready`;
//!   2. read one case name from the serial console;
//!   3. dispatch; on success print `[selftest] <name>: PASS` then reset, on
//!      failure print `[selftest] FAIL: <reason>` then panic.
//!
//! Fault cases do not print PASS: they execute a faulting instruction and the
//! panic handler reports the architecture fault.

/// Enter the ArchTest loop.  Called from `_start` under the `selftest` feature.
pub fn run() -> ! {
    todo!("loongarch64 archtest: emit `[selftest] ready`, read one case name, dispatch")
}
