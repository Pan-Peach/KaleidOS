//! aarch64 CPU backend.
//!
//! # Register conventions (mirrors the RISC-V backend, different carriers)
//!
//! - **per-CPU entry record** = `TPIDR_EL1` (Core-owned local storage pointer +
//!   the logical `CpuId` bound to this hardware CPU);
//! - **interrupt masking** = `DAIF` (`I` bit for IRQ), because Core's
//!   `IrqSaveGuard` needs a save/restore pair;
//! - **timer** = the EL0 virtual timer (`CNTV_*`), programmed locally.
//!
//! # Deliberately `todo!()`
//!
//! - `context_switch` (needs a real AAPCS64 switch frame + assembly);
//! - GICv3 external interrupt `claim`/`complete` and line enable/disable
//!   (nothing routes an external line during boot).
//!
//! `new_context` **is** implemented as a pure record constructor: it builds an
//! architecture-specific execution record (entry + stack) and never executes
//! it.  `core::init` reaches it through `component::containment::init`, which
//! prepares the **executable** abort destination (`task_abort_trampoline`); any
//! actual switch still goes through the `todo!()` `context_switch`.  A
//! boot-construction test therefore proves the record is built, not that task
//! execution works.
//!
//! # GIC naming
//!
//! This backend targets **GICv3** (QEMU `virt` with `-machine gic-version=3`;
//! GICv2 is the older default and has no sysreg CPU interface).  Nothing here
//! programs the GIC yet — the distributor/redistributor bases are discovered
//! from the FDT, not hardcoded, and full claim/complete is the next step.

use crate::cpu::{CpuId, LocalInterruptHandler};
use crate::{Console, CpuArch, InterruptController, ResetType, SystemReset, Timer};
use core::arch::asm;
use core::ptr::NonNull;

/// aarch64 后端类型。
pub struct Aarch64;

/// `PSTATE.I` in the value returned by `MRS Xt, DAIF`.
const PSTATE_I: usize = 1 << 7;
/// PSTATE.DAIF mask (D=bit9, A=bit8, I=bit7, F=bit6).
const DAIF_MASK: usize = 0x3c0;

/// aarch64 上下文记录（骨架；真实布局与 `switch` 汇编一致后钉死偏移）。
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aarch64Context {
    /// 程序计数器。
    pub pc: usize,
    /// 栈指针。
    pub sp: usize,
}

/// 每 CPU 的入口记录，`TPIDR_EL1` 指向它。
///
/// 与 RISC-V 的 `CpuEntry` 同职责：Core 只往里存（逻辑 id + 本地存储基址），
/// arch 只读出来解析「我是谁 / 我的本地存储在哪」。
#[repr(C)]
#[derive(Clone, Copy)]
struct CpuEntry {
    core_base: usize,
    logical_id: usize,
}

impl CpuEntry {
    const fn empty() -> Self {
        Self {
            core_base: 0,
            logical_id: 0,
        }
    }
}

static mut PER_CPU: [CpuEntry; crate::MAX_CPUS] = [CpuEntry::empty(); crate::MAX_CPUS];

/// 逻辑 CPU `i` 的入口记录地址。
fn entry_ptr(i: usize) -> *mut CpuEntry {
    // SAFETY: 只取静态数组元素地址（不创建引用）。
    unsafe { core::ptr::addr_of_mut!(PER_CPU[i]) }
}

#[inline]
fn read_tpidr_el1() -> usize {
    let value: usize;
    // SAFETY: 只读系统寄存器；无内存 / 栈 / 标志位副作用。
    unsafe {
        asm!("mrs {}, tpidr_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

#[inline]
fn read_daif() -> usize {
    let value: usize;
    // SAFETY: 只读 PSTATE 字段；无内存 / 栈副作用。
    unsafe {
        asm!("mrs {}, daif", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// `ISR_EL1`: bit 7 = an IRQ is pending (bit 6 = FIQ, bit 8 = SError).
#[inline]
fn read_isr_el1() -> usize {
    let value: usize;
    // SAFETY: 只读系统寄存器；无内存 / 栈 / 标志位副作用。
    unsafe {
        asm!("mrs {}, isr_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

impl CpuArch for Aarch64 {
    type Context = Aarch64Context;
    type IrqFlags = usize;

    fn context_switch(_from: &mut Self::Context, _to: &Self::Context) {
        todo!("aarch64: AAPCS64 context switch (callee-saved set + TPIDR_EL0 slot)")
    }

    fn new_context(entry: usize, stack_top: usize) -> Self::Context {
        // Builds an execution record; it does not execute anything.  Two
        // distinct caller obligations:
        //  - fresh execution (real entry + stack): must be entered through the
        //    (still `todo!()`) `context_switch`;
        //  - save-only placeholder `(0, 0)` used by Core as the outgoing anchor
        //    for "no current task": never entered as a destination.
        // `core::init` prepares the containment abort context through the first
        // kind: its entry is the real abort trampoline, a panic-recovery
        // destination — not inert metadata.  Building it here does not prove
        // task execution works.
        Aarch64Context {
            pc: entry,
            sp: stack_top,
        }
    }

    fn init_cpu() {
        // Keep every exception masked while the vector table is installed; the
        // Core turns IRQs on explicitly with `enable_irq` at the very end.
        unsafe {
            asm!(
                "msr daifset, #0xf",
                options(nomem, nostack, preserves_flags)
            );
        }
        // Enable FP/SIMD at EL1 (`CPACR_EL1.FPEN = 0b11`).  Rust code for this
        // target may use SIMD registers for bulk moves and in function
        // prologues (`str d8, [sp, ...]`); at reset FPEN is 0 and every such
        // access traps as EC 0x07.
        unsafe {
            let cpacr: usize;
            asm!("mrs {}, cpacr_el1", out(reg) cpacr, options(nomem, nostack, preserves_flags));
            asm!("msr cpacr_el1, {}", in(reg) cpacr | (0b11 << 20), options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
        }
        super::trap::init();
    }

    fn enable_irq() {
        // Clear PSTATE.I only: the global IRQ gate.
        unsafe {
            asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags));
        }
    }

    fn disable_irq() -> Self::IrqFlags {
        let flags = read_daif();
        // SAFETY: register write only; masked state is restored by `restore_irq`.
        unsafe {
            asm!(
                "msr daifset, #0xf",
                options(nomem, nostack, preserves_flags)
            );
        }
        flags
    }

    fn restore_irq(flags: Self::IrqFlags) {
        // SAFETY: register write only; the caller passes the value it observed
        // in `disable_irq` (nested guards restore the outer state).
        unsafe {
            asm!("msr daif, {}", in(reg) flags & DAIF_MASK, options(nomem, nostack, preserves_flags));
        }
    }

    fn wait_for_interrupt() {
        // Raw idle hint: plain WFI.  It may return spuriously (or never, with
        // no pending wake event) and does not touch DAIF — the documented raw
        // primitive contract.  The atomic check→idle protocol is `atomic_idle`.
        // SAFETY: hint instruction; no memory / stack side effects.
        unsafe {
            asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }

    unsafe fn atomic_idle(flags: Self::IrqFlags) {
        if flags & PSTATE_I != 0 {
            // Caller entered with IRQs masked: per the contract there is no
            // enabled wake source, so sleeping could be forever.  Restore and
            // return without idling.
            Self::restore_irq(flags);
            return;
        }
        // An IRQ already pending at the boundary must be delivered by the
        // caller's trap path, not chosen-away by idle.
        if read_isr_el1() & PSTATE_I != 0 {
            Self::restore_irq(flags);
            return;
        }
        // Keep IRQs **masked** across WFI.  AArch64 WFI wakes on a *pending*
        // IRQ even while `PSTATE.I` masks it, so a wakeup arriving now stays
        // pending and is taken only after `restore_irq` unmasks below — no
        // enable→sleep gap.  (Never WFE: there is no event protocol.)
        // SAFETY: hint instruction; IRQs remain masked here and the caller
        // holds no interrupt-path lock.
        unsafe {
            asm!(
                "dsb sy",
                "wfi",
                "isb",
                options(nomem, nostack, preserves_flags)
            );
        }
        Self::restore_irq(flags);
    }

    fn current_cpu() -> Option<CpuId> {
        let entry = read_tpidr_el1();
        if entry == 0 {
            return None;
        }
        // SAFETY: non-zero means `install_per_cpu_base` published this CPU's
        // record pointer; only that function writes it.
        let record = unsafe { &*(entry as *const CpuEntry) };
        Some(CpuId::from_raw(record.logical_id))
    }

    fn per_cpu_base() -> Option<NonNull<()>> {
        let entry = read_tpidr_el1();
        if entry == 0 {
            return None;
        }
        // SAFETY: see `current_cpu`; `core_base` is a Core-provided pointer.
        let record = unsafe { &*(entry as *const CpuEntry) };
        NonNull::new(record.core_base as *mut ())
    }

    unsafe fn install_per_cpu_base(cpu: CpuId, base: NonNull<()>) {
        let i = cpu.raw();
        assert!(
            i < crate::MAX_CPUS,
            "logical CpuId {} exceeds arch MAX_CPUS {}",
            i,
            crate::MAX_CPUS
        );
        // SAFETY: trait contract — this CPU, IRQs masked, before online; the
        // record is a static slot owned by this CPU.
        let entry = entry_ptr(i);
        unsafe {
            (*entry).core_base = base.as_ptr() as usize;
            (*entry).logical_id = i;
            // Publish the record pointer; `current_cpu` / `per_cpu_base` read it.
            asm!(
                "msr tpidr_el1, {}",
                in(reg) entry as usize,
                options(nostack, preserves_flags),
            );
        }
    }
}

impl Timer for Aarch64 {
    fn init_cpu() -> Result<(), crate::TimerError> {
        // Disarm: push the comparator out of reach and mask the output.
        // `CNTV_CTL_EL0 = 0` (disabled), `CNTV_CVAL_EL0 = u64::MAX`.
        unsafe {
            asm!("msr cntv_ctl_el0, {}", in(reg) 0u64, options(nostack, preserves_flags));
            asm!("msr cntv_cval_el0, {}", in(reg) u64::MAX, options(nostack, preserves_flags));
        }
        Ok(())
    }

    fn now() -> u64 {
        let value: u64;
        // SAFETY: read-only counter; the virtual counter is accessible from EL1
        // (and EL2 with E2H=0) by default.
        unsafe {
            asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack, preserves_flags));
        }
        value
    }

    fn set_deadline(deadline: u64) -> Result<(), crate::TimerError> {
        // Program the comparator **and** re-enable the timer output.  Writing
        // only `CNTV_CVAL_EL0` is not enough after `cancel_deadline` (which
        // clears `CNTV_CTL_EL0.ENABLE`): a second one-shot would never fire.
        // `ENABLE=1, IMASK=0` makes the condition real.
        // SAFETY: register writes only; the comparator is a 64-bit counter
        // value and CTL is the architectural enable/mask pair.
        unsafe {
            asm!("msr cntv_cval_el0, {}", in(reg) deadline, options(nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
            asm!("msr cntv_ctl_el0, {}", in(reg) 1u64, options(nostack, preserves_flags));
        }
        Ok(())
    }

    fn cancel_deadline() {
        // SAFETY: register writes only; disable then push the comparator away.
        unsafe {
            asm!("msr cntv_ctl_el0, {}", in(reg) 0u64, options(nostack, preserves_flags));
            asm!("msr cntv_cval_el0, {}", in(reg) u64::MAX, options(nostack, preserves_flags));
        }
    }

    fn register_timer_handler(handler: LocalInterruptHandler) {
        super::trap::register_timer_handler(handler);
    }

    fn enable_timer_interrupt() -> Result<(), crate::TimerError> {
        // Unmask the *local* virtual timer condition (`ENABLE=1, IMASK=0`),
        // then report the truth: delivery to the CPU additionally needs the
        // GICv3 PPI 27 route (redistributor + group enable), which is not
        // brought up.  Returning `Ok` here would make Core publish timer
        // readiness for a callback that can never run.
        unsafe {
            asm!("msr cntv_ctl_el0, {}", in(reg) 1u64, options(nostack, preserves_flags));
        }
        Err(crate::TimerError::DeliveryUnavailable)
    }
}

impl InterruptController for Aarch64 {
    type Config = ();
    type Claim = ();

    unsafe fn configure(_config: ()) -> Result<(), crate::smp::InitError> {
        todo!("aarch64: configure GICv3 distributor/redistributor from discovered MMIO windows")
    }

    fn init_cpu() -> Result<(), crate::smp::InitError> {
        // GICv3 CPU-interface bring-up (`ICC_SRE_EL1` / `ICC_IGRPEN1_EL1` /
        // PMR, redistributor wake) is `todo!()`; the Core only needs this to
        // succeed, and no external line is enabled during boot.
        Ok(())
    }

    fn enable(_line: u32) {
        todo!("aarch64: enable an external interrupt line (GICv3 ISENABLER / GICD_IROUTER)")
    }

    fn disable(_line: u32) {
        todo!("aarch64: disable an external interrupt line (GICv3 ICENABLER)")
    }

    fn claim() -> Option<Self::Claim> {
        todo!("aarch64: acknowledge an interrupt (ICC_IAR1_EL1)")
    }

    fn claim_line(_claim: &Self::Claim) -> u32 {
        todo!("aarch64: map the acknowledged INTID to a line")
    }

    fn complete(_claim: Self::Claim) {
        todo!("aarch64: deactivate the interrupt (ICC_EOIR1_EL1)")
    }

    fn register_external_handler(handler: LocalInterruptHandler) {
        super::trap::register_external_handler(handler);
    }

    fn enable_external_interrupt() {
        todo!("aarch64: unmask external interrupts on this CPU (GICv3 ICC_IGRPEN1_EL1)")
    }
}

impl Console for Aarch64 {
    fn write_byte(byte: u8) {
        super::console::write_byte(byte)
    }

    fn getc() -> Option<u8> {
        super::console::getc()
    }
}

impl SystemReset for Aarch64 {
    fn system_reset(reset_type: ResetType) -> ! {
        // PSCI SYSTEM_OFF powers QEMU down; SYSTEM_RESET restarts it (without
        // `-no-reboot` QEMU reboots instead of exiting, so it is never used for
        // the selftest exit).  QEMU `virt` emulates PSCI via HVC when booting
        // `-kernel` without firmware.
        let function = match reset_type {
            ResetType::Shutdown => super::encoding::PSCI_SYSTEM_OFF,
            ResetType::ColdReboot | ResetType::WarmReboot => super::encoding::PSCI_SYSTEM_RESET,
        };
        // SAFETY: PSCI call; x0 = function id, conduit = HVC (QEMU `virt`
        // without firmware).  If PSCI is unavailable the loop parks the CPU.
        unsafe {
            asm!(
                "hvc #0",
                "1:",
                "wfi",
                "b 1b",
                in("x0") function as u64,
                options(nostack, noreturn),
            );
        }
    }
}
