//! aarch64 exception entry and dispatch.
//!
//! Mirrors `riscv/trap/`: a 2 KiB-aligned vector table (16 entries x 128 B) is
//! installed with `VBAR_EL*`, then Rust dispatch routes timer / external / IPI
//! events to the Core-registered handlers under the **logical** `CpuId`.
//!
//! # Current scope (bring-up)
//!
//! Every vector entry branches to [`aarch64_default_exception_handler`], which
//! prints `ESR`/`FAR`/`ELR` and panics — enough to diagnose ANY unexpected
//! exception instead of hanging silently.  The registered timer/external/IPI
//! callbacks are stored by [`register_timer_handler`] / [`register_external_handler`]
//! / [`register_ipi_handler`] and are dispatched by [`dispatch_timer`] /
//! [`dispatch_external`] / [`dispatch_ipi`]; no vector currently reaches those
//! dispatch points (IRQs stay masked, and GICv3 delivery is `todo!()`), so an
//! interrupt that arrives during bring-up is reported as an exception.
//!
//! [`dispatch_external`] takes the already-acknowledged **INTID** plus the
//! logical `CpuId`: the future GICv3 path owns ack/classification (timer PPI /
//! SGI / device) and deactivation (`ICC_EOIR1_EL1`), converts the INTID to a
//! logical IRQ number, and only then calls the Core callback.

use crate::cpu::{CpuId, ExternalIrqHandler, LocalInterruptHandler};
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Minimal exception frame placeholder (skeleton).
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// Exception Syndrome Register value.
    pub esr: usize,
    /// Fault Address Register value.
    pub far: usize,
    /// Exception Link Register value.
    pub elr: usize,
}

global_asm!(
    r#"
.section .text
.balign 2048
.global aarch64_vector_table
.type aarch64_vector_table, %function
aarch64_vector_table:
    /* 16 slots x 128 B: Sync/IRQ/FIQ/SError for each of the four contexts
       (Current EL SP0, Current EL SPx, Lower EL AArch64, Lower EL AArch32).
       Slot base is 0x200 for "Current EL with SPx" (our EL1/EL2 state). */
    .rept 16
    .balign 128
    b aarch64_default_exception_handler
    .endr
.size aarch64_vector_table, . - aarch64_vector_table
"#
);

unsafe extern "C" {
    fn aarch64_vector_table();
}

/// Current exception level (1/2/3) read from `CurrentEL`.
fn current_el() -> usize {
    let value: usize;
    // SAFETY: read-only system register, always accessible at EL >= 1.
    unsafe {
        asm!("mrs {}, currentel", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value >> 2
}

/// Install the vector table and exception entry on the current CPU.
pub fn init() {
    // Install into the VBAR that matches the level we actually run at.  QEMU
    // `-kernel` enters at EL1, but accepting EL2/EL3 keeps a fault diagnosable
    // instead of jumping through a stale table.
    unsafe {
        match current_el() {
            1 => {
                asm!("msr vbar_el1, {}", in(reg) aarch64_vector_table as *const () as usize, options(nostack))
            }
            2 => {
                asm!("msr vbar_el2, {}", in(reg) aarch64_vector_table as *const () as usize, options(nostack))
            }
            3 => {
                asm!("msr vbar_el3, {}", in(reg) aarch64_vector_table as *const () as usize, options(nostack))
            }
            _ => {}
        }
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
}

/// Default vector entry: report the architectural fault and halt.
///
/// Called from every slot of the vector table; never returns.
#[unsafe(no_mangle)]
extern "C" fn aarch64_default_exception_handler() -> ! {
    let esr: usize;
    let far: usize;
    let elr: usize;
    // SAFETY: read-only system registers.  The table is installed for the
    // current EL, so these EL1 registers are the ones that trapped for the
    // supported entry level (EL1).
    unsafe {
        asm!("mrs {}, esr_el1", out(reg) esr, options(nomem, nostack, preserves_flags));
        asm!("mrs {}, far_el1", out(reg) far, options(nomem, nostack, preserves_flags));
        asm!("mrs {}, elr_el1", out(reg) elr, options(nomem, nostack, preserves_flags));
    }
    panic!(
        "unhandled aarch64 exception: EL{} ESR={:#x} FAR={:#x} ELR={:#x}",
        current_el(),
        esr,
        far,
        elr
    );
}

/// 当前执行 CPU 的**逻辑**身份（未绑定的 CPU 是不变式破坏，绝不回退 CPU0）。
fn current_logical_cpu() -> CpuId {
    use crate::CpuArch;
    crate::CpuImpl::current_cpu().expect("current CPU is not bound during interrupt dispatch")
}

/// 时钟中断回调（Core 在 `timer::init` 时注册 `timer::on_trap`）。
static TIMER_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// Store the Core timer handler.
pub fn register_timer_handler(handler: LocalInterruptHandler) {
    TIMER_HANDLER.store(handler as usize, Ordering::Release);
}

/// Invoke the registered timer handler with the current logical `CpuId`.
pub fn dispatch_timer() {
    let address = TIMER_HANDLER.load(Ordering::Acquire);
    assert!(address != 0, "timer interrupt handler is not registered");
    // SAFETY: 注册方保证签名与 `LocalInterruptHandler` 一致（单一注册入口）。
    let handler: LocalInterruptHandler = unsafe { core::mem::transmute(address) };
    handler(current_logical_cpu());
}

/// 外部中断回调（Core 在 `irq::init` 时注册 `crate::irq::on_irq`）。
static EXTERNAL_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// Store the Core external-interrupt callback: `(logical CpuId, logical IRQ)`.
pub fn register_external_handler(handler: ExternalIrqHandler) {
    EXTERNAL_HANDLER.store(handler as usize, Ordering::Release);
}

/// Invoke the registered external callback for one acknowledged interrupt.
///
/// This is only the dispatch seam: the (future) GICv3 delivery path owns
/// ack/classification (timer PPI / SGI / device), EOI/deactivate, and the
/// INTID → logical IRQ mapping before handing the identity to Core.  No vector
/// reaches this seam during bring-up (IRQs stay masked; GICv3 delivery is
/// `todo!()`), so `intid` is forwarded as the interrupt identity as-is.
pub fn dispatch_external(cpu: CpuId, intid: u32) {
    let address = EXTERNAL_HANDLER.load(Ordering::Acquire);
    assert!(address != 0, "external interrupt handler is not registered");
    // SAFETY: 注册方保证签名与 `ExternalIrqHandler` 一致（单一注册入口）。
    let handler: ExternalIrqHandler = unsafe { core::mem::transmute(address) };
    handler(cpu, intid);
}

/// IPI（GIC SGI）回调（Core 在 `smp::init` 时注册 `ipi::ipi_interrupt`）。
static IPI_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// Store the Core IPI handler (single registration).
pub fn register_ipi_handler(handler: LocalInterruptHandler) {
    IPI_HANDLER.store(handler as usize, Ordering::Release);
}

/// Invoke the registered IPI handler with the current logical `CpuId`.
pub fn dispatch_ipi() {
    let address = IPI_HANDLER.load(Ordering::Acquire);
    assert!(address != 0, "IPI handler is not registered");
    // SAFETY: 注册方保证签名与 `LocalInterruptHandler` 一致（单一注册入口）。
    let handler: LocalInterruptHandler = unsafe { core::mem::transmute(address) };
    handler(current_logical_cpu());
}
