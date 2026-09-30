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
/// Sanity cap for the Multiboot2 info block (a real block is a few KiB): a
/// bogus `total_size` must not drive an unbounded tag scan.
const MB2_MAX_TOTAL_SIZE: usize = 64 * 1024;
/// Size of one `hvm_memmap_table_entry` (`base: u64, length: u64, type: u32,
/// reserved: u32`), the PVH memory-map entry.
const PVH_MEMMAP_ENTRY_SIZE: usize = 24;
/// Sanity cap for a PVH memory map: more entries than this is firmware garbage.
const PVH_MAX_MEMMAP_ENTRIES: usize = 4096;
/// Plausibility bound for boot-info physical addresses.  The x86 boot protocol
/// hands over low-RAM pointers, so anything at/above 4 GiB is bogus.
const BOOT_INFO_MAX_PA: usize = 0x1_0000_0000;
/// Size of the fixed part of PVH `hvm_start_info` that boot consumes
/// (`magic, version, flags, nr_modules, modlist_paddr, cmdline_paddr,
/// rsdp_paddr, memmap_paddr, memmap_entries, reserved` = 56 bytes).
const HVM_START_INFO_SIZE: usize = 56;

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

    /// The usable bank that fully contains `[start, end)`, if any.
    fn containing(&self, start: usize, end: usize) -> Option<MemoryRegion> {
        self.regions[..self.count]
            .iter()
            .find(|region| {
                region.base <= start
                    && region
                        .base
                        .checked_add(region.size)
                        .is_some_and(|limit| end <= limit)
            })
            .copied()
    }
}

/// Capacity of [`BootData::exclusions`]: PVH needs two (start_info + memmap),
/// MB2 needs one (the whole information block).
const BOOT_EXCLUSION_CAPACITY: usize = 2;

/// Boot-info discovery result: usable RAM regions plus the firmware-owned
/// boot payload extents the early arena must never overlap (a MB2 information
/// block, or PVH `hvm_start_info` + its separate memory-map array).
struct BootData {
    regions: RegionTable,
    exclusions: [MemoryRegion; BOOT_EXCLUSION_CAPACITY],
    exclusion_count: usize,
}

impl BootData {
    fn empty() -> Self {
        Self {
            regions: RegionTable::new(),
            exclusions: [MemoryRegion { base: 0, size: 0 }; BOOT_EXCLUSION_CAPACITY],
            exclusion_count: 0,
        }
    }

    /// Record one boot payload extent.  Overflow is a programming error (the
    /// callsites are fixed and bounded) and must not silently drop an
    /// exclusion the arena would then be free to overwrite.
    fn exclude(&mut self, base: usize, size: usize) {
        assert!(
            self.exclusion_count < self.exclusions.len(),
            "boot payload exclusion capacity exceeded"
        );
        self.exclusions[self.exclusion_count] = MemoryRegion { base, size };
        self.exclusion_count += 1;
    }
}

/// Multiboot2 information structure -> usable RAM regions.
///
/// Layout: `total_size: u32, reserved: u32`, then 8-byte-aligned tags
/// (`type: u32, size: u32`, size includes the header).  Tag 6 is the memory
/// map (`entry_size: u32, entry_version: u32`, then 24-byte entries:
/// `base: u64, length: u64, type: u32, reserved: u32`); tag 4 is basic
/// meminfo (`mem_lower: u32, mem_upper: u32` in KiB) and is only a fallback.
///
/// The whole validated information block (including embedded ACPI tags) is
/// excluded from the early arena: boot keeps consuming it while discovering.
fn multiboot2_regions(info_pa: usize) -> BootData {
    let mut table = BootData::empty();
    // The info pointer comes from the boot protocol: validate it before any read.
    if info_pa == 0 || !info_pa.is_multiple_of(8) || info_pa >= BOOT_INFO_MAX_PA {
        kernel::log!(
            "discovery",
            "MB2: implausible info pointer {:#x}; no RAM regions",
            info_pa
        );
        return table;
    }
    let total_size = read_u32(info_pa) as usize;
    if !(8..=MB2_MAX_TOTAL_SIZE).contains(&total_size) {
        kernel::log!(
            "discovery",
            "MB2: implausible total_size {:#x}; no RAM regions",
            total_size
        );
        return table;
    }
    let Some(end) = info_pa.checked_add(total_size) else {
        kernel::log!("discovery", "MB2: info block address wraps; no RAM regions");
        return table;
    };
    table.exclude(info_pa, total_size);
    let mut cursor = info_pa + 8;
    let mut basic_mem_upper_kib = 0u32;

    while cursor + 8 <= end {
        let tag_type = read_u32(cursor);
        let tag_size = read_u32(cursor + 4) as usize;
        if tag_type == 0 || tag_size < 8 {
            break;
        }
        // The tag must fit inside the info block before anything inside it is
        // used; a truncated final tag is not trusted.
        let tag_end = match cursor.checked_add(tag_size) {
            Some(tail) if tail <= end => tail,
            _ => {
                kernel::log!(
                    "discovery",
                    "MB2: tag {:#x} (size {:#x}) overruns the info block; stop",
                    tag_type,
                    tag_size
                );
                break;
            }
        };
        if tag_type == 6 && tag_size >= 16 {
            // Memory map: the count is derived only after the 16-byte
            // entry_size/entry_version header is known to fit.
            let entry_size = read_u32(cursor + 8) as usize;
            if entry_size >= PVH_MEMMAP_ENTRY_SIZE {
                let count = (tag_size - 16) / entry_size;
                let mut index = 0;
                while index < count {
                    let entry = cursor
                        .checked_add(16)
                        .and_then(|base| base.checked_add(index * entry_size));
                    let Some(entry) = entry else { break };
                    let Some(entry_end) = entry.checked_add(PVH_MEMMAP_ENTRY_SIZE) else {
                        break;
                    };
                    if entry_end > tag_end {
                        break;
                    }
                    if read_u32(entry + 16) == MEMORY_AVAILABLE {
                        table.regions.push(read_u64(entry), read_u64(entry + 8));
                    }
                    index += 1;
                }
            }
        } else if tag_type == 4 && tag_size >= 16 {
            basic_mem_upper_kib = read_u32(cursor + 12);
        }
        cursor = match tag_end.checked_add(7) {
            Some(next) => next & !7,
            None => break,
        };
    }

    // Fallback: no mmap tag (or no usable entry) -> basic meminfo 1 MiB..
    if table.regions.count == 0 && basic_mem_upper_kib > 0 {
        table
            .regions
            .push(0x10_0000, basic_mem_upper_kib as u64 * 1024);
    }
    table
}

/// PVH `hvm_start_info` (version >= 1) -> usable RAM regions.
///
/// Layout: `magic: u32, version: u32, flags: u32, nr_modules: u32,
/// modlist_paddr: u64, cmdline_paddr: u64, rsdp_paddr: u64,
/// memmap_paddr: u64, memmap_entries: u32, reserved: u32`; the memmap entries
/// are the same 24-byte `hvm_memmap_table_entry` shape as MB2.
///
/// Boot keeps consuming `hvm_start_info` and its separate memory-map array, so
/// both fixed extents are excluded from the early arena.
fn pvh_regions(info_pa: usize) -> BootData {
    let mut table = BootData::empty();
    let version = read_u32(info_pa + 4);
    if version < 1 {
        kernel::log!(
            "discovery",
            "PVH: unsupported start_info version {}; no RAM regions",
            version
        );
        return table;
    }
    // The map pointer and count come from firmware: validate both before any
    // entry read, and keep the whole map below the 4 GiB plausibility bound.
    let memmap_pa = read_u64(info_pa + 40) as usize;
    let entries = read_u32(info_pa + 48) as usize;
    if memmap_pa == 0 || entries == 0 || entries > PVH_MAX_MEMMAP_ENTRIES {
        kernel::log!(
            "discovery",
            "PVH: implausible memmap (addr {:#x}, {} entries); no RAM regions",
            memmap_pa,
            entries
        );
        return table;
    }
    let Some(memmap_end) = entries
        .checked_mul(PVH_MEMMAP_ENTRY_SIZE)
        .and_then(|size| memmap_pa.checked_add(size))
        .filter(|end| *end <= BOOT_INFO_MAX_PA)
    else {
        kernel::log!(
            "discovery",
            "PVH: memmap (addr {:#x}, {} entries) out of range; no RAM regions",
            memmap_pa,
            entries
        );
        return table;
    };
    table.exclude(info_pa, HVM_START_INFO_SIZE);
    table.exclude(memmap_pa, memmap_end - memmap_pa);
    for index in 0..entries {
        // `index * 24` cannot overflow: `entries <= 4096` and `memmap_end`
        // above already proved `memmap_pa + entries * 24` fits.
        let entry = memmap_pa + index * PVH_MEMMAP_ENTRY_SIZE;
        if entry + PVH_MEMMAP_ENTRY_SIZE > memmap_end {
            break;
        }
        if read_u32(entry + 16) == MEMORY_AVAILABLE {
            table.regions.push(read_u64(entry), read_u64(entry + 8));
        }
    }
    table
}

/// Boot protocol dispatch: EAX magic first (Multiboot2), then the info magic
/// (`hvm_start_info` for the PVH fallback).
fn discover_regions(magic: usize, info_pa: usize) -> BootData {
    // The info pointer is firmware-provided: refuse an implausible one instead
    // of dereferencing it (the protocol pointers are low RAM on x86).
    if info_pa == 0 || info_pa >= BOOT_INFO_MAX_PA {
        panic!(
            "boot discovery: implausible boot info pointer {:#x}",
            info_pa
        );
    }
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

    let boot = discover_regions(magic, info_pa);
    let image_start = core::ptr::addr_of!(__kernel_start) as usize;
    let image_end = core::ptr::addr_of!(__kernel_end) as usize;
    let image_size = image_end - image_start;
    let image = MemoryRegion {
        base: image_start,
        size: image_size,
    };

    if boot.regions.count == 0 {
        panic!("boot discovery: no usable RAM region in boot info");
    }
    let Some(bank) = boot.regions.containing(image_start, image_end) else {
        panic!(
            "boot discovery: no RAM region covers the kernel image {:#x}-{:#x}",
            image_start, image_end
        );
    };

    // Early-memory seam（无堆）：在包含镜像的那个 bank 里，排除镜像与 boot
    // payload（PVH start_info / memmap 或整个 MB2 信息块），选出最大的页对齐
    // 连续间隙作为 arena。固定 RegionTable 仍是机器 inventory，**不是** arena。
    let arena = kernel::memory::select_arena(bank, image, |emit| {
        emit(image)?;
        for excluded in &boot.exclusions[..boot.exclusion_count] {
            emit(*excluded)?;
        }
        Ok(())
    })
    .unwrap_or_else(|error| panic!("boot discovery: arena selection failed: {}", error));
    kernel::log!(
        "bootstrap",
        "image {:#x}-{:#x} ({} KiB), bank {:#x}+{} KiB, {} RAM region(s)",
        image_start,
        image_end,
        image_size / 1024,
        bank.base,
        bank.size / 1024,
        boot.regions.count
    );
    kernel::log!(
        "bootstrap",
        "early arena {:#x}-{:#x} ({} KiB), boot payload exclusions {}",
        arena.base,
        arena.base + arena.size,
        arena.size / 1024,
        boot.exclusion_count
    );

    // SAFETY: arena 是 boot 从可用 RAM 中选出的连续窗口——镜像（含 boot 栈 /
    // 页表 / 静态缓冲）与全部 boot payload 都已被排除；boot 单 CPU、中断未开、
    // 尚无其它分配者。
    if let Err(error) = unsafe { kernel::memory::early_init(arena) } {
        panic!("early memory init failed: {}", error);
    }

    let mut memory_regions = [MemoryRegion { base: 0, size: 0 }; MAX_REGIONS];
    memory_regions[..boot.regions.count]
        .copy_from_slice(&boot.regions.regions[..boot.regions.count]);

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
        mem_count: boot.regions.count,
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
