//! x86_64 boot: entry (`entry.S`) -> long mode -> boot-info discovery ->
//! `MachineInfo` -> `kernel::init` -> ArchTest harness / Core Monitor.
//!
//! # Boot protocol (verified against the host QEMU)
//!
//! The host QEMU's loader is 4.2 (`qemu-system-x86_64 --version` prints 5.2,
//! but the shipped binary is Ubuntu's 4.2 build): it implements **Multiboot 1
//! only** and its MB1 ELF path hard-rejects `EM_X86_64` ("Cannot load x86-64
//! image, give a 32bit one").  Multiboot 2 is not implemented at all.
//!
//! The working `-kernel` fallback is the **PVH (Xen HVM direct boot) note**,
//! which QEMU 4.2 enables by default on q35: an ELF64 image carrying
//! `XEN_ELFNOTE_PHYS32_ENTRY` is loaded and entered in 32-bit protected mode
//! with `%ebx` = physical `hvm_start_info` (magic `0x336ec578`, version 1,
//! with an e820 memory map).
//!
//! This image carries both descriptors: the Multiboot2 header required by the
//! boot contract (first 32 KiB, 8-byte aligned; inert on this QEMU) and the
//! PVH note that actually boots.  `bootstrap_main` dispatches on the entry
//! magic, so a QEMU with Multiboot2 support would use the MB2 tag path.
//!
//! # Discovery -> MachineInfo
//!
//! - memory: usable (type 1) regions from the PVH/MB2 24-byte memory map,
//!   with MB2 basic-meminfo as the fallback when no mmap tag is present;
//! - CPU: exactly one CPU (the BSP), hardware id = CPUID leaf 1 initial APIC
//!   id; AP discovery (ACPI MADT) is not part of this bring-up;
//! - devices: the 16550 UART (PIO) is registered so the machine dump is real.
//!
//! `reserved` is the linked image range (`__kernel_start`..`__kernel_end`).

#![no_std]
#![no_main]

use arch::{Console, CpuArch};
use core::arch::global_asm;
use core::panic::PanicInfo;
use kernel::machine::{
    CompatStr, CpuInfo, DeviceDescriptor, HardwareCpuId, IoSpace, MachineInfo, MemoryRegion,
    MAX_CPUS,
};

#[cfg(feature = "selftest")]
#[path = "selftest.rs"]
mod selftest;

// `options(att_syntax)`: this toolchain's `global_asm!` defaults to
// Intel syntax; the entry assembly is written in AT&T (GAS) syntax.
global_asm!(include_str!("entry.S"), options(att_syntax));

unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Multiboot2 boot magic (EAX at entry).
const MB2_BOOT_MAGIC: usize = 0x36d7_6289;
/// Xen HVM `hvm_start_info` magic (`hvm_start_info.magic` for PVH entry).
const HVM_START_MAGIC: u32 = 0x336e_c578;
/// Guest memory map "usable RAM" entry type (MB2 mmap and hvm_memmap).
const MEMORY_AVAILABLE: u32 = 1;
/// Boot-side staging capacity, matching `MachineInfo.memory_regions`.
const MAX_REGIONS: usize = 16;

/// Raw (identity-mapped) boot-info reads.
fn read_u32(address: usize) -> u32 {
    // SAFETY: callers pass addresses from the boot protocol (RAM, identity
    // mapped by `entry.S`).
    unsafe { core::ptr::read_unaligned(address as *const u32) }
}

fn read_u64(address: usize) -> u64 {
    // SAFETY: see [`read_u32`].
    unsafe { core::ptr::read_unaligned(address as *const u64) }
}

/// Staging buffer for the discovered usable RAM regions.
struct RegionTable {
    regions: [MemoryRegion; MAX_REGIONS],
    count: usize,
}

impl RegionTable {
    fn new() -> Self {
        Self {
            regions: [MemoryRegion { base: 0, size: 0 }; MAX_REGIONS],
            count: 0,
        }
    }

    fn push(&mut self, base: u64, size: u64) {
        if size == 0 || base > usize::MAX as u64 || size > usize::MAX as u64 {
            return;
        }
        if self.count >= MAX_REGIONS {
            return;
        }
        self.regions[self.count] = MemoryRegion {
            base: base as usize,
            size: size as usize,
        };
        self.count += 1;
    }

    fn covers(&self, start: usize, end: usize) -> bool {
        self.regions[..self.count]
            .iter()
            .any(|region| region.base <= start && end <= region.base + region.size)
    }
}

/// Multiboot2 information structure -> usable RAM regions.
///
/// Layout: `total_size: u32, reserved: u32`, then 8-byte-aligned tags
/// (`type: u32, size: u32`, size includes the header).  Tag 6 is the memory
/// map (`entry_size: u32, entry_version: u32`, then 24-byte entries:
/// `base: u64, length: u64, type: u32, reserved: u32`); tag 4 is basic
/// meminfo (`mem_lower: u32, mem_upper: u32` in KiB) and is only a fallback.
fn multiboot2_regions(info_pa: usize) -> RegionTable {
    let mut table = RegionTable::new();
    let total_size = read_u32(info_pa) as usize;
    let end = info_pa.saturating_add(total_size);
    let mut cursor = info_pa.saturating_add(8);
    let mut basic_mem_upper_kib = 0u32;

    while cursor + 8 <= end {
        let tag_type = read_u32(cursor);
        let tag_size = read_u32(cursor + 4) as usize;
        if tag_type == 0 || tag_size < 8 {
            break;
        }
        if tag_type == 6 {
            if let Some(count) = (tag_size - 16).checked_div(read_u32(cursor + 8) as usize) {
                let entry_size = read_u32(cursor + 8) as usize;
                if entry_size >= 24 {
                    for index in 0..count.min(MAX_REGIONS * 4) {
                        let entry = cursor + 16 + index * entry_size;
                        if entry + 24 > end {
                            break;
                        }
                        if read_u32(entry + 16) == MEMORY_AVAILABLE {
                            table.push(read_u64(entry), read_u64(entry + 8));
                        }
                    }
                }
            }
        } else if tag_type == 4 {
            basic_mem_upper_kib = read_u32(cursor + 12);
        }
        cursor += (tag_size + 7) & !7;
    }

    // Fallback: no mmap tag (or no usable entry) -> basic meminfo 1 MiB..
    if table.count == 0 && basic_mem_upper_kib > 0 {
        table.push(0x10_0000, basic_mem_upper_kib as u64 * 1024);
    }
    table
}

/// PVH `hvm_start_info` (version >= 1) -> usable RAM regions.
///
/// Layout: `magic: u32, version: u32, flags: u32, nr_modules: u32,
/// modlist_paddr: u64, cmdline_paddr: u64, rsdp_paddr: u64,
/// memmap_paddr: u64, memmap_entries: u32, reserved: u32`; the memmap entries
/// are the same 24-byte `hvm_memmap_table_entry` shape as MB2.
fn pvh_regions(info_pa: usize) -> RegionTable {
    let mut table = RegionTable::new();
    let version = read_u32(info_pa + 4);
    if version >= 1 {
        let memmap_pa = read_u64(info_pa + 40) as usize;
        let entries = read_u32(info_pa + 48) as usize;
        for index in 0..entries.min(MAX_REGIONS * 4) {
            let entry = memmap_pa + index * 24;
            if read_u32(entry + 16) == MEMORY_AVAILABLE {
                table.push(read_u64(entry), read_u64(entry + 8));
            }
        }
    }
    table
}

/// Boot protocol dispatch: EAX magic first (Multiboot2), then the info magic
/// (`hvm_start_info` for the PVH fallback).
fn discover_regions(magic: usize, info_pa: usize) -> RegionTable {
    if magic == MB2_BOOT_MAGIC {
        kernel::log!(
            "discovery",
            "boot protocol: Multiboot2 (info @ {:#x})",
            info_pa
        );
        return multiboot2_regions(info_pa);
    }
    let info_magic = read_u32(info_pa);
    if info_magic == HVM_START_MAGIC {
        kernel::log!(
            "discovery",
            "boot protocol: PVH (start_info @ {:#x})",
            info_pa
        );
        return pvh_regions(info_pa);
    }
    panic!(
        "unknown boot protocol: eax={:#x}, info_magic={:#x} @ {:#x}",
        magic, info_magic, info_pa
    );
}

/// The BSP's initial APIC id from CPUID leaf 1 EBX[31:24] (readable without
/// enabling the APIC).
fn boot_apic_id() -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        // CPUID leaf 1 is architectural on x86_64.
        let leaf = core::arch::x86_64::__cpuid(1);
        leaf.ebx >> 24
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

/// The discovered 16550 UART as an `IoSpace::Pio` device descriptor.
fn uart_device() -> DeviceDescriptor {
    let mut device = DeviceDescriptor::empty();
    device.space = IoSpace::Pio {
        base: arch::x86_64::console::COM1 as usize,
        size: 8,
    };
    device.irq = Some(4);
    device.compatibles[0] = CompatStr::from_bytes(b"ns16550a");
    device.compat_count = 1;
    device
}

/// Entry from `entry.S` (64-bit, identity mapped, `.bss` cleared).
#[unsafe(no_mangle)]
extern "C" fn bootstrap_main(magic: usize, info_pa: usize) -> ! {
    arch::CpuImpl::init_cpu();
    // Core's `current_cpu()` must resolve before `kernel::init` consumes it.
    // The Core-owned per-CPU storage is not defined yet, so publish a dangling
    // placeholder (Core only stores and passes it, never dereferences here).
    // SAFETY: this CPU, interrupts disabled, before going online.
    unsafe {
        <arch::CpuImpl as arch::CpuArch>::install_per_cpu_base(
            kernel::machine::CpuId::from_raw(0),
            core::ptr::NonNull::dangling(),
        );
    }

    kernel::log!("bootstrap", "KaleidOS x86_64 bootstrap");
    kernel::log!("bootstrap", "========================================");

    let discovered = discover_regions(magic, info_pa);
    let image_start = core::ptr::addr_of!(__kernel_start) as usize;
    let image_end = core::ptr::addr_of!(__kernel_end) as usize;
    let image_size = image_end - image_start;

    if discovered.count == 0 {
        panic!("boot discovery: no usable RAM region in boot info");
    }
    if !discovered.covers(image_start, image_end) {
        panic!(
            "boot discovery: no RAM region covers the kernel image {:#x}-{:#x}",
            image_start, image_end
        );
    }

    let mut memory_regions = [MemoryRegion { base: 0, size: 0 }; MAX_REGIONS];
    memory_regions[..discovered.count].copy_from_slice(&discovered.regions[..discovered.count]);

    let mut cpu_info = [CpuInfo {
        boot_cpu: false,
        hardware_id: HardwareCpuId::from_raw(0),
    }; MAX_CPUS];
    cpu_info[0] = CpuInfo {
        boot_cpu: true,
        hardware_id: HardwareCpuId::from_raw(boot_apic_id() as u64),
    };

    let mut devices = [DeviceDescriptor::empty(); 26];
    devices[0] = uart_device();

    let info = MachineInfo {
        boot_hardware_id: cpu_info[0].hardware_id,
        // TSC frequency is not discoverable via CPUID 0x15/0x16 on `qemu64`,
        // so the timebase is **unknown** (`0` convention, not fabricated).
        // No timer-derived period is used on this port: `Timer` reports
        // `Unsupported`/`DeliveryUnavailable` and Core polls.
        timebase_frequency: 0,
        // AP discovery (ACPI MADT) is not part of this bring-up: the BSP is
        // the only CPU Core may see, and it must be logical CPU0 with
        // `boot_cpu = true`.
        cpu_count: 1,
        cpu_info,
        mem_count: discovered.count,
        memory_regions,
        dev_count: 1,
        devices,
    };

    kernel::log!(
        "bootstrap",
        "image {:#x}-{:#x} ({} KiB), {} RAM region(s), boot apic id {}",
        image_start,
        image_end,
        image_size / 1024,
        info.mem_count,
        info.boot_hardware_id.raw()
    );
    kernel::log!("bootstrap", "MachineInfo dump:");
    kernel::printk!("{:#?}\n", info);

    let reserved = [MemoryRegion {
        base: image_start,
        size: image_size,
    }];
    if let Err(error) = kernel::init(&info, &reserved) {
        panic!("core init failed: {}", error);
    }
    kernel::log!("bootstrap", "BOOT CORE OK");

    // Local sources are unmasked by `kernel::init`; open the global gate last.
    arch::CpuImpl::enable_irq();

    #[cfg(feature = "selftest")]
    {
        selftest::run(&info);
    }

    #[cfg(not(feature = "selftest"))]
    {
        kernel::monitor::run();
    }
}

/// Fatal panic path: direct polled console output (no Core locks / heap),
/// then park.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut writer = DirectWriter;
    let _ = core::fmt::write(&mut writer, format_args!("\nPANIC: {}\n", info));
    loop {
        core::hint::spin_loop();
    }
}

/// Direct COM1 sink used by the panic handler.
struct DirectWriter;

impl core::fmt::Write for DirectWriter {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        for byte in value.bytes() {
            arch::ConsoleImpl::write_byte(byte);
        }
        Ok(())
    }
}
