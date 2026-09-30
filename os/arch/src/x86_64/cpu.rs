//! x86_64 CPU backend.
//!
//! Interface shape matches the `riscv` / `fake` backends.  Carrier choices:
//!
//! - **per-CPU base**: `IA32_GS_BASE` MSR points at this CPU's [`CpuEntry`]
//!   (logical id + Core local storage pointer).  `swapgs` is a privilege-entry
//!   mechanism and is not used yet.
//!
//! # Bring-up scope
//!
//! Implemented: CPU init (IDT + legacy PIC remap with all lines masked),
//! global interrupt enable, IRQ-save/restore, raw `hlt` + atomic idle, TSC
//! time source, per-CPU identity, reset, and the structural `new_context`
//! record (Core's containment/sched init constructs placeholder contexts
//! during `kernel::init`).
//! Deliberately absent: any deadline timer (the PIT is not started) and the
//! APIC/IOAPIC **routing** path — `Timer` reports `Unsupported`/
//! `DeliveryUnavailable` instead of faking delivery.
//! Explicit `todo!()`: the register-level task context **switch** assembly and
//! the APIC/IOAPIC line routing.

use super::encoding as enc;
use crate::cpu::{CpuId, ExternalIrqHandler, LocalInterruptHandler};
use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};

/// x86_64 backend type.
pub struct X86_64;

/// x86_64 context record (bring-up placeholder shape).
///
/// The real switch assembly (`context::switch`, still `todo!()`) must save /
/// restore this exact layout.
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86_64Context {
    /// Instruction pointer.
    pub rip: usize,
    /// Stack pointer.
    pub rsp: usize,
}

/// Per-CPU entry record: the GS-base target.
#[repr(C)]
struct CpuEntry {
    /// Core-assigned logical CPU id.
    logical_id: usize,
    /// Core local storage base (opaque to arch).
    core_base: usize,
}

impl CpuEntry {
    const fn empty() -> Self {
        Self {
            logical_id: 0,
            core_base: 0,
        }
    }
}

static mut ENTRIES: [CpuEntry; crate::MAX_CPUS] = [const { CpuEntry::empty() }; crate::MAX_CPUS];

// ---------------------------------------------------------------------------
// MSR / flag primitives
// ---------------------------------------------------------------------------

fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: `rdmsr` is a plain register transfer; the MSR number is
    // architectural (see `encoding`).
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((high as u64) << 32) | low as u64
}

fn wrmsr(msr: u32, value: u64) {
    // SAFETY: `wrmsr` writes the requested architectural MSR; callers pass
    // fixed MSR numbers and values (GS/FS base).
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// The current logical CPU id, or `CpuId(0)` before the GS-base record is
/// installed (the BSP is the only execution context at that point).
pub(crate) fn current_logical_cpu() -> CpuId {
    let base = rdmsr(enc::MSR_GS_BASE);
    if base == 0 {
        return CpuId::from_raw(0);
    }
    // SAFETY: GS base is only written by `install_per_cpu_base`, always with
    // the address of an `ENTRIES` slot.
    let id = unsafe { (*(base as *const CpuEntry)).logical_id };
    CpuId::from_raw(id)
}

impl CpuArch for X86_64 {
    type Context = X86_64Context;
    type IrqFlags = usize;

    fn context_switch(_from: &mut Self::Context, _to: &Self::Context) {
        todo!("x86_64: task context switch (assembly)")
    }

    fn new_context(entry: usize, stack_top: usize) -> Self::Context {
        // Builds an execution record; it does not execute anything.  Two
        // distinct caller obligations:
        //  - fresh execution (real task entry + stack): must be entered through
        //    the (still `todo!()`) `context_switch`;
        //  - save-only placeholder `(0, 0)` used by Core as the outgoing anchor
        //    for "no current task": never entered as a destination.
        // Containment's abort context is the fresh-execution kind: a real
        // panic-recovery destination (Core switches to it), not inert data.
        // A boot-construction test only proves the record is built; it does not
        // prove task execution works.
        X86_64Context {
            rip: entry,
            rsp: stack_top,
        }
    }

    fn init_cpu() {
        super::console::init();
        super::trap::init();
    }

    fn enable_irq() {
        // SAFETY: `sti` only sets RFLAGS.IF; local sources were unmasked first.
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    }

    fn disable_irq() -> Self::IrqFlags {
        let flags: usize;
        // SAFETY: `pushfq`/`pop` snapshot RFLAGS, `cli` clears IF; the pair is
        // the standard irq-save primitive.
        unsafe {
            core::arch::asm!(
                "pushfq",
                "pop {}",
                "cli",
                out(reg) flags,
                options(nomem, nostack),
            );
        }
        flags
    }

    fn restore_irq(flags: Self::IrqFlags) {
        // `popfq` restores IF (and the rest of the snapshot) exactly; a nested
        // guard therefore restores the state it observed.
        // SAFETY: `flags` came from `disable_irq` on this CPU.
        unsafe {
            core::arch::asm!(
                "push {}",
                "popfq",
                in(reg) flags,
                options(nomem, nostack),
            );
        }
    }

    fn wait_for_interrupt() {
        // Raw idle hint: `hlt` resumes only on an interrupt that can wake the
        // CPU (or reset/SMI).  Nothing here arms a timer or changes IF, and no
        // interrupt source is currently unmasked on this bring-up, so this can
        // return spuriously or never return — exactly the documented raw
        // primitive contract.  The atomic check→idle protocol is `atomic_idle`.
        // SAFETY: `hlt` is a hint; it resumes at the next wake event.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }

    unsafe fn atomic_idle(flags: Self::IrqFlags) {
        const RFLAGS_IF: usize = 1 << 9;
        if flags & RFLAGS_IF == 0 {
            // Caller had interrupts disabled: there is no enabled wakeup
            // source, so never halt (it would be forever).  Restore the exact
            // flags anyway (IF is already 0; this keeps the contract uniform).
            Self::restore_irq(flags);
            return;
        }
        // Contiguous enable/halt sequence: the STI interrupt shadow defers an
        // already-pending interrupt until after the following instruction
        // (the HLT), so the halt cannot miss a wakeup in the enable→sleep
        // gap.  `cli` re-masks before restoring the caller's exact flags.
        // SAFETY: `sti`/`hlt`/`cli` only touch RFLAGS and CPU state; the
        // caller's contract guarantees no interrupt-path lock is held.
        unsafe {
            core::arch::asm!("sti", "hlt", "cli", options(nomem, nostack),);
        }
        Self::restore_irq(flags);
    }

    fn current_cpu() -> Option<CpuId> {
        if rdmsr(enc::MSR_GS_BASE) == 0 {
            None
        } else {
            Some(current_logical_cpu())
        }
    }

    fn per_cpu_base() -> Option<core::ptr::NonNull<()>> {
        let base = rdmsr(enc::MSR_GS_BASE);
        if base == 0 {
            return None;
        }
        // SAFETY: see `current_logical_cpu`.
        let core_base = unsafe { (*(base as *const CpuEntry)).core_base };
        core::ptr::NonNull::new(core_base as *mut ())
    }

    unsafe fn install_per_cpu_base(cpu: CpuId, base: core::ptr::NonNull<()>) {
        let index = cpu.raw();
        assert!(
            index < crate::MAX_CPUS,
            "logical CpuId {} exceeds arch MAX_CPUS {}",
            index,
            crate::MAX_CPUS
        );
        // SAFETY: trait contract — this runs on the bound CPU with interrupts
        // disabled, before that CPU goes online; only this CPU reads it back.
        let entry = unsafe { core::ptr::addr_of_mut!(ENTRIES[index]) };
        // SAFETY: `entry` is a live static slot; GS/KERNEL_GS base are
        // architectural MSRs, and no TLS exists in this image.
        unsafe {
            (*entry).logical_id = index;
            (*entry).core_base = base.as_ptr() as usize;
            wrmsr(enc::MSR_GS_BASE, entry as u64);
            wrmsr(enc::MSR_KERNEL_GS_BASE, entry as u64);
        }
    }
}

impl Timer for X86_64 {
    fn init_cpu() -> Result<(), crate::TimerError> {
        // The TSC is readable (see `now`), but there is **no deadline timer
        // bring-up** on this port: LAPIC/TSC-deadline programming is not
        // implemented and the legacy PIT is deliberately not started (starting
        // it would be a hidden periodic wakeup pretending to be a deadline
        // source).  `Unsupported` is the honest answer — Core then falls back
        // to polling for the console instead of arming a deadline.
        Err(crate::TimerError::Unsupported)
    }

    fn now() -> u64 {
        // SAFETY: `rdtsc` is always available on x86_64 (QEMU implements it);
        // it reads a monotonic per-CPU counter.
        unsafe { core::arch::x86_64::_rdtsc() }
    }

    fn set_deadline(_deadline: u64) -> Result<(), crate::TimerError> {
        // One-shot deadline programming is a `todo!()` boundary (LAPIC timer /
        // TSC-deadline bring-up).  Report the failure instead of pretending:
        // Core only calls this after `init_cpu`/`enable_timer_interrupt`
        // succeeded, which they never do on this port.
        Err(crate::TimerError::Unsupported)
    }

    fn cancel_deadline() {
        // No programmed deadline exists; see `set_deadline`.
    }

    fn register_timer_handler(handler: LocalInterruptHandler) {
        super::trap::register_timer_handler(handler);
    }

    fn enable_timer_interrupt() -> Result<(), crate::TimerError> {
        // Delivery needs a real deadline source plus its route/CPU interface.
        // Neither exists (see `init_cpu`); the PIT tick was the only timer IRQ
        // and it is no longer started.
        Err(crate::TimerError::DeliveryUnavailable)
    }
}

impl InterruptController for X86_64 {
    type Config = ();

    unsafe fn configure(_config: ()) -> Result<(), crate::smp::InitError> {
        // Legacy PIC/PIT are configured by `trap::init`; there is no external
        // controller configuration to apply yet.
        Ok(())
    }

    fn init_cpu() -> Result<(), crate::smp::InitError> {
        Ok(())
    }

    fn enable(_line: u32) {
        todo!(
            "x86_64: IOAPIC line routing not implemented (no external line is enabled during boot)"
        )
    }

    fn disable(_line: u32) {
        todo!(
            "x86_64: IOAPIC line routing not implemented (no external line is enabled during boot)"
        )
    }

    fn register_external_handler(handler: ExternalIrqHandler) {
        super::trap::register_external_handler(handler);
    }

    fn enable_external_interrupt() {
        // External delivery (LAPIC/IOAPIC) is not brought up; the PIC IRQ0
        // tick is the only interrupt source and needs no per-CPU unmask.
    }
}

impl Console for X86_64 {
    fn write_byte(byte: u8) {
        super::console::write_byte(byte)
    }

    fn getc() -> Option<u8> {
        super::console::getc()
    }
}

impl SystemReset for X86_64 {
    fn system_reset(reset_type: ResetType) -> ! {
        match reset_type {
            ResetType::Shutdown => {
                // ACPI S5 soft-off: q35 puts PM1a_CNT at I/O port 0x604
                // (PM1a_EVT at 0x600, +4 = control).
                //
                // SLP_TYP is **0** here, not 5: QEMU's DSDT exports
                // `_S5` with PM1a_CNT.SLP_TYP = 0 (`hw/i386/acpi-build.c`)
                // and its PM implementation issues the shutdown request for
                // `(val >> 10) & 7 == 0` with SLP_EN set
                // (`hw/acpi/core.c: acpi_pm1_cnt_write`).  Writing the
                // real-hardware SLP_TYP=5 value is a silent no-op on QEMU.
                // A portable implementation would read the FADT _S5 value
                // once ACPI parsing exists (no ACPI discovery yet).
                // SAFETY: fixed ACPI I/O port, ring-0, q35 boot platform.
                unsafe { super::console::outw(0x604, 1 << 13) };
            }
            ResetType::ColdReboot | ResetType::WarmReboot => {
                // 8042 keyboard-controller reset line: pulse bit 0.  QEMU
                // restarts the machine (no `-no-reboot` on the runner).
                // SAFETY: fixed legacy I/O port.
                unsafe { super::console::outb(0x64, 0xFE) };
            }
        }
        loop {
            // SAFETY: if the platform ignored the request, park the CPU rather
            // than execute further with a dying machine state.
            unsafe { core::arch::asm!("cli", "hlt", options(nomem, nostack)) };
        }
    }
}
