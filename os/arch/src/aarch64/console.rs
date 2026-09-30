//! aarch64 early console transport: polled PL011 UART on the QEMU `virt` board.
//!
//! QEMU `virt` maps its first PL011 at `0x0900_0000`.  The boot path runs with
//! the MMU off (identity), so the MMIO window is dereferenced directly by a
//! volatile pointer; no page table or `Device` mapping is required for this
//! bring-up stage.
//!
//! The transport is **polled** (no RX interrupt, `IMSC = 0`): the Core console
//! contract is `getc() -> Option<u8>`, and IRQ delivery is explicitly out of
//! scope here (GICv3 bring-up is `todo!()`).
//!
//! This module compiles on the host too (`mod aarch64` is `cfg(any(test, ...))`);
//! only the register access is gated on `target_arch = "aarch64"` so host
//! builds on any architecture stay green.

/// PL011 MMIO window on QEMU `virt`.
#[cfg(target_arch = "aarch64")]
const UART_BASE: usize = 0x0900_0000;

/// Data register (read = RX byte, write = TX byte).
#[cfg(target_arch = "aarch64")]
const UART_DR: usize = 0x00;
/// Flag register (bit 4 = RXFE, bit 5 = TXFF).
#[cfg(target_arch = "aarch64")]
const UART_FR: usize = 0x18;
/// Integer baud rate divisor.
#[cfg(target_arch = "aarch64")]
const UART_IBRD: usize = 0x24;
/// Fractional baud rate divisor.
#[cfg(target_arch = "aarch64")]
const UART_FBRD: usize = 0x28;
/// Line control (8N1 + FIFO enable = 0x70).
#[cfg(target_arch = "aarch64")]
const UART_LCR_H: usize = 0x2c;
/// Control register (UARTEN | TXE | RXE = 0x301).
#[cfg(target_arch = "aarch64")]
const UART_CR: usize = 0x30;
/// Interrupt mask (0 = fully polled).
#[cfg(target_arch = "aarch64")]
const UART_IMSC: usize = 0x38;
/// Interrupt clear register.
#[cfg(target_arch = "aarch64")]
const UART_ICR: usize = 0x44;
/// Receive FIFO empty (`FR.RXFE`).
#[cfg(target_arch = "aarch64")]
const FR_RXFE: u32 = 1 << 4;
/// Transmit FIFO full (`FR.TXFF`).
#[cfg(target_arch = "aarch64")]
const FR_TXFF: u32 = 1 << 5;

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn mmio_read(offset: usize) -> u32 {
    // SAFETY: caller guarantees `UART_BASE + offset` is a live MMIO register
    // (QEMU `virt` PL011 window, identity-mapped while the MMU is off).
    unsafe { core::ptr::read_volatile((UART_BASE + offset) as *const u32) }
}

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn mmio_write(offset: usize, value: u32) {
    // SAFETY: see `mmio_read`.
    unsafe { core::ptr::write_volatile((UART_BASE + offset) as *mut u32, value) };
}

/// Initialize the PL011 for polled 8N1 operation.
///
/// QEMU's model tolerates writes before this runs, but a real PL011 powers up
/// disabled (`CR = 0`), so the boot path calls this once before its first byte.
/// Interrupts stay masked: the console is polled end to end.
pub fn init() {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        // Disable while reconfiguring, then clear any latched interrupt.
        mmio_write(UART_CR, 0);
        mmio_write(UART_ICR, 0x7ff);
        // Divisor for the standard 115200 8N1 rate; QEMU ignores the baud rate,
        // and a future platform layer should take it from the clock/FDT.
        mmio_write(UART_IBRD, 1);
        mmio_write(UART_FBRD, 0);
        mmio_write(UART_LCR_H, 0x70); // 8 bits, no parity, 1 stop, FIFOs on
        mmio_write(UART_IMSC, 0); // polled: no TX/RX interrupts
        mmio_write(UART_CR, 0x301); // UARTEN | TXE | RXE
    }
}

/// Write one byte to the early console (blocks while the TX FIFO is full).
pub fn write_byte(byte: u8) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        while mmio_read(UART_FR) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        mmio_write(UART_DR, byte as u32);
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = byte;
    }
}

/// Read one byte from the early console, if one is available.
pub fn getc() -> Option<u8> {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        if mmio_read(UART_FR) & FR_RXFE != 0 {
            return None;
        }
        Some((mmio_read(UART_DR) & 0xff) as u8)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        None
    }
}
