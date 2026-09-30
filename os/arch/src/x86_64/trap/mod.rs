//! x86_64 exception / interrupt entry and dispatch.
//!
//! Mirrors `riscv/trap/`: entry assembly saves a frame (`entry.S`), then Rust
//! dispatch routes timer / external / IPI events to the Core-registered
//! handlers under the **logical** `CpuId` (see [`crate::cpu::LocalInterruptHandler`]).
//!
//! # Bring-up scope
//!
//! - A full 256-entry IDT with a diagnosable default fault path (prints vector /
//!   error code / RIP and halts) so a fault is never a silent triple fault.
//! - The legacy 8259 PIC is remapped to vectors 0x20..0x2F with **every line
//!   masked**.  The remap only keeps the PIC's default vectors away from the
//!   architectural exception range; no interrupt source is enabled.
//! - There is **no timer interrupt**: the 8254 PIT is deliberately not started
//!   (a periodic tick would be a hidden wakeup pretending to be a deadline
//!   source).  `Timer` reports `Unsupported`/`DeliveryUnavailable`, and Core
//!   falls back to polling for the console.
//! - The local APIC / IOAPIC **routing** (`InterruptController::enable` /
//!   `disable`) is **not** brought up: no external line is enabled during boot,
//!   so those methods stay explicit `todo!()`.  The external *dispatch* shape
//!   is nevertheless honest: PIC vectors `0x20..=0x2F` are EOI'd at the PIC
//!   and handed to the Core callback as the logical IRQ number
//!   (`vector - 0x20`), with no fake `claim`.  PIC IRQ0 (vector 0x20) is the
//!   legacy timer line and stays with the timer callback.
//!
//! # Frame layout
//!
//! `entry.S` pushes, low to high: `rax rcx rdx rsi rdi r8 r9 r10 r11 rbx`,
//! then `vector`, then `error_code`, then the CPU frame (`rip cs rflags rsp
//! ss`).  [`TrapFrame`] must match that order byte for byte; the offset
//! assertions below fail the build if they drift.

use crate::cpu::{ExternalIrqHandler, LocalInterruptHandler};
use core::arch::{asm, global_asm};
use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};

// `options(att_syntax)`: this toolchain's `global_asm!` defaults to
// Intel syntax; the entry assembly is written in AT&T (GAS) syntax.
global_asm!(include_str!("entry.S"), options(att_syntax));

/// IDT vector of the (future) PIT / deadline timer after the PIC remap.
///
/// Nothing raises it today (see the module docs); the dispatch below is kept so
/// a real deadline source (LAPIC timer / TSC-deadline) has a landing pad.
pub const TIMER_VECTOR: u8 = 0x20;

/// Last PIC vector after the remap (`0x20..=0x2F` = PIC IRQ0..15).
const PIC_VECTOR_LAST: usize = 0x2F;
/// First slave-PIC vector (PIC IRQ8): those also need an EOI to the slave.
const PIC_SLAVE_VECTOR: usize = 0x28;

/// Number of architectural exception vectors (0..31).
const EXCEPTION_VECTORS: usize = 32;

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;
const PIC_EOI: u8 = 0x20;

/// Trap frame as built by `entry.S` (see module docs for the layout contract).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    pub rax: usize,
    pub rcx: usize,
    pub rdx: usize,
    pub rsi: usize,
    pub rdi: usize,
    pub r8: usize,
    pub r9: usize,
    pub r10: usize,
    pub r11: usize,
    pub rbx: usize,
    /// Interrupt vector / exception number.
    pub vector: usize,
    /// Hardware error code where the architecture supplies one, else 0.
    pub error_code: usize,
    pub rip: usize,
    pub cs: usize,
    pub rflags: usize,
    pub rsp: usize,
    pub ss: usize,
}

// Layout is ABI with entry.S: the pushed register block is 10 words, then
// vector, then error code, then the CPU frame.
const _: () = {
    assert!(core::mem::offset_of!(TrapFrame, vector) == 10 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(TrapFrame, error_code) == 11 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(TrapFrame, rip) == 12 * core::mem::size_of::<usize>());
};

/// 64-bit IDT gate.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

/// Present, DPL=0, 64-bit interrupt gate (IF cleared on entry).
const IDT_INTERRUPT_GATE: u8 = 0x8E;

/// GDT selector of the 64-bit kernel code segment (see `os/boot/x86_64/src/entry.S`).
const KERNEL_CODE_SELECTOR: u16 = 0x10;

const IDT_ENTRY_MISSING: IdtEntry = IdtEntry {
    offset_low: 0,
    selector: 0,
    ist: 0,
    type_attr: 0,
    offset_mid: 0,
    offset_high: 0,
    zero: 0,
};

fn gate(stub: usize) -> IdtEntry {
    IdtEntry {
        offset_low: stub as u16,
        selector: KERNEL_CODE_SELECTOR,
        ist: 0,
        type_attr: IDT_INTERRUPT_GATE,
        offset_mid: (stub >> 16) as u16,
        offset_high: (stub >> 32) as u32,
        zero: 0,
    }
}

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

#[repr(C, align(16))]
struct Idt {
    entries: [IdtEntry; 256],
}

static mut IDT: Idt = Idt {
    entries: [IDT_ENTRY_MISSING; 256],
};

static TIMER_HANDLER: AtomicUsize = AtomicUsize::new(0);
static EXTERNAL_HANDLER: AtomicUsize = AtomicUsize::new(0);
static IPI_HANDLER: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" {
    // Stub address table emitted by `entry.S` (`x86_trap_entry`, called from
    // the same assembly, is defined below in Rust and needs no declaration).
    static x86_isr_stub_table: [usize; 256];
}

/// Install the IDT and remap the legacy PIC (all lines masked) on this CPU.
pub fn init() {
    // SAFETY: BSP early boot, single-threaded, interrupts still disabled by
    // the boot path; IDT and stub table live in this image.
    unsafe {
        let stubs = core::ptr::addr_of!(x86_isr_stub_table) as *const usize;
        let entries = core::ptr::addr_of_mut!(IDT.entries) as *mut IdtEntry;
        for i in 0..256 {
            entries.add(i).write(gate(stubs.add(i).read()));
        }
        let pointer = IdtPointer {
            limit: (core::mem::size_of::<Idt>() - 1) as u16,
            base: core::ptr::addr_of!(IDT) as u64,
        };
        asm!(
            "lidt [{}]",
            in(reg) core::ptr::addr_of!(pointer),
            options(nostack),
        );
    }
    init_legacy_pic();
}

/// Remap the 8259 pair to vectors 0x20..0x2F and mask every line.
///
/// The remap is required because the PIC's reset vectors overlap the
/// architectural exception range; masking all lines is required because this
/// port brings up **no** interrupt source (the PIT is deliberately not
/// started).
fn init_legacy_pic() {
    // SAFETY: fixed legacy PC I/O ports; runs once during early boot with
    // interrupts disabled and no other user of the PIC.
    unsafe {
        // ICW1: begin initialization, edge triggered, cascade.
        super::console::outb(PIC1_COMMAND, 0x11);
        super::console::outb(PIC2_COMMAND, 0x11);
        // ICW2: vector offsets 0x20 (master) / 0x28 (slave).
        super::console::outb(PIC1_DATA, TIMER_VECTOR);
        super::console::outb(PIC2_DATA, 0x28);
        // ICW3: slave on IRQ2 of the master.
        super::console::outb(PIC1_DATA, 0x04);
        super::console::outb(PIC2_DATA, 0x02);
        // ICW4: 8086/88 mode.
        super::console::outb(PIC1_DATA, 0x01);
        super::console::outb(PIC2_DATA, 0x01);
        // OCW1: mask every line (no interrupt source is enabled).
        super::console::outb(PIC1_DATA, 0xFF);
        super::console::outb(PIC2_DATA, 0xFF);
    }
}

/// Store the Core timer handler (called once by `core::timer::init`).
pub fn register_timer_handler(handler: LocalInterruptHandler) {
    TIMER_HANDLER.store(handler as usize, Ordering::Release);
}

/// Store the Core external-interrupt callback: `(logical CpuId, logical IRQ)`.
pub fn register_external_handler(handler: ExternalIrqHandler) {
    EXTERNAL_HANDLER.store(handler as usize, Ordering::Release);
}

/// Store the Core IPI handler.
pub fn register_ipi_handler(handler: LocalInterruptHandler) {
    IPI_HANDLER.store(handler as usize, Ordering::Release);
}

fn load_handler(slot: &AtomicUsize) -> Option<LocalInterruptHandler> {
    let raw = slot.load(Ordering::Acquire);
    if raw == 0 {
        return None;
    }
    // SAFETY: only `register_*` writes these slots, always with a
    // `LocalInterruptHandler` (a function pointer into this same image).
    Some(unsafe { core::mem::transmute::<usize, LocalInterruptHandler>(raw) })
}

/// Invoke the registered timer handler with the current logical `CpuId`.
pub fn dispatch_timer() {
    if let Some(handler) = load_handler(&TIMER_HANDLER) {
        handler(super::cpu::current_logical_cpu());
    }
}

/// Invoke the registered external callback for one PIC line.
///
/// The backend owns ack/EOI: the trap entry already EOI'd the PIC, and `irq` is
/// the **logical** IRQ number (`vector - 0x20`) — never a claim token.
pub fn dispatch_external(irq: u32) {
    let raw = EXTERNAL_HANDLER.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }
    // SAFETY: only `register_external_handler` writes this slot, always with an
    // `ExternalIrqHandler` (a function pointer into this same image).
    let handler: ExternalIrqHandler = unsafe { core::mem::transmute(raw) };
    handler(super::cpu::current_logical_cpu(), irq);
}

/// Invoke the registered IPI handler with the current logical `CpuId`.
pub fn dispatch_ipi() {
    if let Some(handler) = load_handler(&IPI_HANDLER) {
        handler(super::cpu::current_logical_cpu());
    }
}

/// Common Rust trap entry (called from `entry.S`).
///
/// Only the timer vector returns; every fault / unexpected vector is diagnosed
/// and halts.  This is deliberately not a recovery path.
#[unsafe(no_mangle)]
extern "C" fn x86_trap_entry(frame: *const TrapFrame) {
    // SAFETY: `entry.S` always passes the frame pointer it just built.
    let frame = unsafe { &*frame };
    if (TIMER_VECTOR as usize..=PIC_VECTOR_LAST).contains(&frame.vector) {
        // PIC-sourced line: the backend completes it at the PIC *before*
        // dispatching (a slow handler must not lose the next edge, and the
        // handler may re-enable the source).  Vectors >= 0x28 also need the
        // slave EOI.
        //
        // Nothing raises these vectors today (every line is masked and the PIT
        // is deliberately not started); the shape stays honest so a real
        // source has a landing pad.
        // SAFETY: PIC was configured by `init` on this CPU.
        unsafe {
            if frame.vector >= PIC_SLAVE_VECTOR {
                super::console::outb(PIC2_COMMAND, PIC_EOI);
            }
            super::console::outb(PIC1_COMMAND, PIC_EOI);
        }
        if frame.vector == TIMER_VECTOR as usize {
            // PIC IRQ0 is the legacy timer line: the timer callback owns it.
            dispatch_timer();
        } else {
            // External line: Core receives the **logical** IRQ number (not the
            // vector); ack/EOI already happened above.
            dispatch_external((frame.vector - TIMER_VECTOR as usize) as u32);
        }
        return;
    }
    fault_halt(frame);
}

/// Print one diagnostic line and halt.  Used for every exception and for any
/// vector that should not be reachable during this bring-up.
fn fault_halt(frame: &TrapFrame) -> ! {
    let kind = if frame.vector < EXCEPTION_VECTORS {
        "exception"
    } else {
        "unexpected interrupt"
    };
    let mut writer = ConsoleWriter;
    let _ = writeln!(
        writer,
        "\n[trap] x86_64 {} vector={} err={:#x} rip={:#x} rsp={:#x}",
        kind, frame.vector, frame.error_code, frame.rip, frame.rsp
    );
    loop {
        // SAFETY: a trap with no return path must not accept further
        // interrupts; `cli` + `hlt` parks the CPU until reset.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// Direct console sink: the trap path must not touch Core printing/locks.
struct ConsoleWriter;

impl Write for ConsoleWriter {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        for byte in value.bytes() {
            super::console::write_byte(byte);
        }
        Ok(())
    }
}
