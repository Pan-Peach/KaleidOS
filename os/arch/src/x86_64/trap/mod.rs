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
//! - The legacy 8259 PIC is remapped to vectors 0x20..0x2F and the 8254 PIT
//!   channel 0 is started at ~100 Hz on vector [`TIMER_VECTOR`].  This periodic
//!   tick is the wake source for the polled console loop (`hlt` in
//!   [`crate::CpuArch::wait_for_interrupt`]) and feeds Core's timer trap entry.
//! - The local APIC / IOAPIC (claim/complete, line routing) is **not** brought
//!   up: no external line is enabled during boot, so the corresponding
//!   `InterruptController` methods stay explicit `todo!()`.
//!
//! # Frame layout
//!
//! `entry.S` pushes, low to high: `rax rcx rdx rsi rdi r8 r9 r10 r11 rbx`,
//! then `vector`, then `error_code`, then the CPU frame (`rip cs rflags rsp
//! ss`).  [`TrapFrame`] must match that order byte for byte; the offset
//! assertions below fail the build if they drift.

use crate::cpu::LocalInterruptHandler;
use core::arch::{asm, global_asm};
use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};

// `options(att_syntax)`: this toolchain's `global_asm!` defaults to
// Intel syntax; the entry assembly is written in AT&T (GAS) syntax.
global_asm!(include_str!("entry.S"), options(att_syntax));

/// IDT vector of the PIT (IRQ0) after the PIC remap done by [`init`].
pub const TIMER_VECTOR: u8 = 0x20;

/// Number of architectural exception vectors (0..31).
const EXCEPTION_VECTORS: usize = 32;

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;
const PIC_EOI: u8 = 0x20;

const PIT_CHANNEL0: u16 = 0x40;
const PIT_COMMAND: u16 = 0x43;
/// Input clock of the 8254 (Hz).
const PIT_BASE_HZ: u32 = 1_193_182;
/// Periodic tick rate programmed into PIT channel 0.
const PIT_TICK_HZ: u32 = 100;

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

/// Install the IDT and bring up the legacy PIC + PIT on the current CPU.
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
    init_legacy_pic_and_pit();
}

/// Remap the 8259 pair to vectors 0x20..0x2F, unmask only IRQ0, and start the
/// 8254 PIT channel 0 as a ~100 Hz periodic tick.
fn init_legacy_pic_and_pit() {
    // SAFETY: fixed legacy PC I/O ports; runs once during early boot with
    // interrupts disabled and no other user of the PIC/PIT.
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
        // OCW1: unmask only IRQ0 (PIT) on the master; slave fully masked.
        super::console::outb(PIC1_DATA, 0xFE);
        super::console::outb(PIC2_DATA, 0xFF);
        // 8254: channel 0, lo/hi byte, mode 3 (square wave), binary.
        let divisor = (PIT_BASE_HZ / PIT_TICK_HZ) as u16;
        super::console::outb(PIT_COMMAND, 0x36);
        super::console::outb(PIT_CHANNEL0, divisor as u8);
        super::console::outb(PIT_CHANNEL0, (divisor >> 8) as u8);
    }
}

/// Store the Core timer handler (called once by `core::timer::init`).
pub fn register_timer_handler(handler: LocalInterruptHandler) {
    TIMER_HANDLER.store(handler as usize, Ordering::Release);
}

/// Store the Core external-interrupt handler.
pub fn register_external_handler(handler: LocalInterruptHandler) {
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

/// Invoke the registered external-interrupt handler with the current CPU id.
pub fn dispatch_external() {
    if let Some(handler) = load_handler(&EXTERNAL_HANDLER) {
        handler(super::cpu::current_logical_cpu());
    }
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
    if frame.vector == TIMER_VECTOR as usize {
        // Complete the interrupt at the PIC before dispatching so a slow
        // handler cannot lose the next tick; the PIT keeps its period.
        // SAFETY: PIC/PIT were configured by `init` on this CPU.
        unsafe { super::console::outb(PIC1_COMMAND, PIC_EOI) };
        dispatch_timer();
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
