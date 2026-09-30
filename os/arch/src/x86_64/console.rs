//! x86_64 early console transport: polled 16550-compatible UART at COM1.
//!
//! Console is a boot/firmware transport capability, not an ISA primitive
//! (same split as `riscv/console.rs`).  There is no firmware call on the QEMU
//! `-nographic` path, so the backend talks to the 16550 registers directly:
//! QEMU maps a serial port at I/O ports 0x3F8..0x3FF and connects it to stdio.
//!
//! This module also owns the x86 **port I/O primitives** (`in`/`out`), because
//! they are the transport's raw material and the only other users (PIC/PIT in
//! `trap`, ACPI reset in `cpu`) are equally low-level boot plumbing.
//!
//! Host builds on any architecture (`cfg(test)`) must compile this file: the
//! port primitives are `target_arch = "x86_64"`-gated and the non-x86 fallback
//! is inert.  No host test may touch real I/O ports, and none does.

/// COM1 data register (RBR read / THR write; DLL when DLAB=1).
pub const COM1: u16 = 0x3F8;
/// COM1 interrupt enable register (DLM when DLAB=1).
pub const COM1_IER: u16 = COM1 + 1;
/// COM1 FIFO control register (write-only).
pub const COM1_FCR: u16 = COM1 + 2;
/// COM1 line control register.
pub const COM1_LCR: u16 = COM1 + 3;
/// COM1 modem control register.
pub const COM1_MCR: u16 = COM1 + 4;
/// COM1 line status register.
pub const COM1_LSR: u16 = COM1 + 5;

/// LSR bit 0: received data ready.
pub const LSR_DATA_READY: u8 = 1 << 0;
/// LSR bit 5: transmit holding register empty.
pub const LSR_THR_EMPTY: u8 = 1 << 5;

// ---------------------------------------------------------------------------
// Port I/O primitives
// ---------------------------------------------------------------------------

/// Read one byte from an I/O port.
///
/// # Safety
/// Port I/O is unprivileged-only (ring 0); the caller must ensure the port
/// belongs to a device this execution domain may touch.
#[cfg(target_arch = "x86_64")]
pub(crate) unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: caller contract above; `in` is a plain register transfer.
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

/// Write one byte to an I/O port.
///
/// # Safety
/// Same contract as [`inb`].
#[cfg(target_arch = "x86_64")]
pub(crate) unsafe fn outb(port: u16, value: u8) {
    // SAFETY: caller contract above.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Write one 16-bit word to an I/O port.
///
/// # Safety
/// Same contract as [`inb`].
///
/// Host test builds compile this module but never the bare-metal reset path
/// (`cpu.rs`), so the function is legitimately unused there.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
#[cfg(target_arch = "x86_64")]
pub(crate) unsafe fn outw(port: u16, value: u16) {
    // SAFETY: caller contract above.
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Non-x86 host fallback: no I/O ports exist, keep the build compiling.
///
/// This whole fallback exists only so `cfg(test)` builds on non-x86 hosts
/// compile; nothing can call it there, hence the dead-code allowance.
#[allow(dead_code)]
#[cfg(not(target_arch = "x86_64"))]
pub(crate) unsafe fn inb(_port: u16) -> u8 {
    0
}

/// Non-x86 host fallback: see [`inb`].
#[allow(dead_code)]
#[cfg(not(target_arch = "x86_64"))]
pub(crate) unsafe fn outb(_port: u16, _value: u8) {}

/// Non-x86 host fallback: see [`inb`].
#[allow(dead_code)]
#[cfg(not(target_arch = "x86_64"))]
pub(crate) unsafe fn outw(_port: u16, _value: u16) {}

// ---------------------------------------------------------------------------
// UART bring-up
// ---------------------------------------------------------------------------

/// Program COM1 for polled 115200 8N1 operation, interrupts disabled.
///
/// QEMU's serial model is usable without this (it starts in 8N1 and ignores
/// the divisor), but initializing it keeps the backend honest on real 16550s
/// and guarantees the FIFO/interrupt state this polled transport assumes.
pub fn init() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: COM1 is a fixed, reserved PC I/O range; this runs once during
    // early boot on the BSP before any interrupt source is unmasked.
    unsafe {
        outb(COM1_IER, 0x00); // no RX/TX interrupts (polled transport)
        outb(COM1_LCR, 0x80); // DLAB=1 to program the divisor
        outb(COM1, 0x01); // DLL = 1 -> 115200 baud
        outb(COM1_IER, 0x00); // DLM = 0
        outb(COM1_LCR, 0x03); // 8 data bits, no parity, 1 stop bit, DLAB=0
        outb(COM1_FCR, 0xC7); // FIFO on, clear both queues, 14-byte trigger
        outb(COM1_MCR, 0x03); // DTR | RTS
    }
}

// ---------------------------------------------------------------------------
// Console transport
// ---------------------------------------------------------------------------

/// Write one byte to COM1 (polled; blocks until the THR is empty).
pub fn write_byte(byte: u8) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: COM1 was initialized by [`init`] (or QEMU's default state);
        // this only reads LSR and writes THR.
        unsafe {
            let mut spins = 0u32;
            while inb(COM1_LSR) & LSR_THR_EMPTY == 0 {
                core::hint::spin_loop();
                spins += 1;
                if spins > 1_000_000 {
                    // A missing/never-ready UART must not wedge the whole boot:
                    // drop the byte rather than spin forever.
                    return;
                }
            }
            outb(COM1, byte);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = byte;
}

/// Read one byte from COM1 if one is ready (polled, non-blocking).
pub fn getc() -> Option<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: see [`write_byte`].
        unsafe {
            if inb(COM1_LSR) & LSR_DATA_READY != 0 {
                return Some(inb(COM1));
            }
        }
        None
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        None
    }
}
