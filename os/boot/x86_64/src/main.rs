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
//! - devices: the 16550 UART (PIO) is registered so the machine dump is real;
//! - firmware: the retained raw source.  PVH publishes `Acpi { rsdp }` from
//!   `hvm_start_info.rsdp_paddr` (offset 32), MB2 from the ACPI RSDP tag payload
//!   (tag 15 = v2+ preferred, tag 14 = v1 fallback) -- each validated
//!   (signature / checksum / revision-appropriate length) before it is retained.
//!   No valid RSDP is `FirmwareInfo::Static` (this static BSP/UART platform has
//!   no retained firmware description).  The retained RSDP extent is excluded
//!   from the early arena before any memory is admitted.
//!
//! `reserved` is the linked image range (`__kernel_start`..`__kernel_end`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use arch::{Console, CpuArch};
use core::arch::global_asm;
use core::panic::PanicInfo;
use kernel::machine::{
    CpuInfo, DeviceDescriptor, FirmwareInfo, HardwareCpuId, InterruptResource, InterruptSpecifier,
    IoSpace, MachineInfo, MemoryRegion,
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
/// ACPI RSDP signature (8 bytes, trailing space included).
const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";
/// Canonical RSDP length for revision 0 (RSDT, first-20-byte checksum).
const RSDP_V1_SIZE: usize = 20;
/// Canonical RSDP length for revision >= 2 (XSDT + extended checksum).
const RSDP_V2_SIZE: usize = 36;
/// Multiboot2 tag type: ACPI old RSDP (a v1 RSDP copy embedded in the payload).
const MB2_TAG_ACPI_OLD_RSDP: u32 = 14;
/// Multiboot2 tag type: ACPI new RSDP (a v2+ RSDP copy embedded in the payload).
const MB2_TAG_ACPI_NEW_RSDP: u32 = 15;

/// Raw (identity-mapped) boot-info reads.
fn read_u8(address: usize) -> u8 {
    // SAFETY: callers pass addresses from the boot protocol (RAM, identity
    // mapped by `entry.S`).
    unsafe { core::ptr::read_volatile(address as *const u8) }
}

fn read_u32(address: usize) -> u32 {
    // SAFETY: callers pass addresses from the boot protocol (RAM, identity
    // mapped by `entry.S`).
    unsafe { core::ptr::read_unaligned(address as *const u32) }
}

fn read_u64(address: usize) -> u64 {
    // SAFETY: see [`read_u32`].
    unsafe { core::ptr::read_unaligned(address as *const u64) }
}

/// Capacity of the boot-payload exclusion set: PVH needs three (start_info +
/// its separate memmap + the retained RSDP), MB2 needs two (the whole
/// information block + the RSDP tag copy, already inside that block).
const BOOT_EXCLUSION_CAPACITY: usize = 3;

/// Firmware-owned boot payload extents the early arena must never overlap
/// (a MB2 information block, or PVH `hvm_start_info` + its separate map).
///
/// Fixed and tiny by construction: these are the *boot payload* records, not
/// the machine RAM inventory (which is owned/`Vec`-built after the seam).
#[derive(Clone, Copy)]
struct ExclusionTable {
    exclusions: [MemoryRegion; BOOT_EXCLUSION_CAPACITY],
    count: usize,
}

impl ExclusionTable {
    fn empty() -> Self {
        Self {
            exclusions: [MemoryRegion { base: 0, size: 0 }; BOOT_EXCLUSION_CAPACITY],
            count: 0,
        }
    }

    /// Record one boot payload extent.  Overflow is a programming error (the
    /// callsites are fixed and bounded) and must not silently drop an
    /// exclusion the arena would then be free to overwrite.
    fn exclude(&mut self, base: usize, size: usize) {
        assert!(
            self.count < self.exclusions.len(),
            "boot payload exclusion capacity exceeded"
        );
        self.exclusions[self.count] = MemoryRegion { base, size };
        self.count += 1;
    }
}

/// Boot protocol flavour decided once from the entry registers.
#[derive(Clone, Copy)]
enum BootProtocol {
    Multiboot2,
    Pvh,
}

/// Identify the boot protocol from the entry magic / info block.  Logs once
/// and panics on an unknown protocol (boot cannot guess how to read RAM).
fn detect_protocol(magic: usize, info_pa: usize) -> BootProtocol {
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
        return BootProtocol::Multiboot2;
    }
    let info_magic = read_u32(info_pa);
    if info_magic == HVM_START_MAGIC {
        kernel::log!(
            "discovery",
            "boot protocol: PVH (start_info @ {:#x})",
            info_pa
        );
        return BootProtocol::Pvh;
    }
    panic!(
        "unknown boot protocol: eax={:#x}, info_magic={:#x} @ {:#x}",
        magic, info_magic, info_pa
    );
}

/// Hand one firmware `(base, size)` pair to `sink` when it is representable;
/// returns whether a region was emitted (zero size / overflow skipped).
fn emit_region(sink: &mut dyn FnMut(MemoryRegion), base: u64, size: u64) -> bool {
    if size == 0 || base > usize::MAX as u64 || size > usize::MAX as u64 {
        return false;
    }
    sink(MemoryRegion {
        base: base as usize,
        size: size as usize,
    });
    true
}

/// Walk every usable RAM region the boot protocol describes, calling `sink`.
///
/// **Allocation-free and re-runnable**: the validated boot payload stays
/// outside the early arena, so the pre-seam pass (find the image bank, pick
/// the arena) and the post-seam pass (build the owned inventory with `Vec`)
/// reuse the same parsing.  Returns the number of usable regions seen.
fn walk_usable_regions(
    protocol: BootProtocol,
    info_pa: usize,
    sink: &mut dyn FnMut(MemoryRegion),
    excluded: &mut ExclusionTable,
) -> usize {
    match protocol {
        BootProtocol::Multiboot2 => multiboot2_regions(info_pa, sink, excluded),
        BootProtocol::Pvh => pvh_regions(info_pa, sink, excluded),
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
fn multiboot2_regions(
    info_pa: usize,
    sink: &mut dyn FnMut(MemoryRegion),
    excluded: &mut ExclusionTable,
) -> usize {
    let mut count = 0usize;
    // The info pointer comes from the boot protocol: validate it before any read.
    if info_pa == 0 || !info_pa.is_multiple_of(8) || info_pa >= BOOT_INFO_MAX_PA {
        kernel::log!(
            "discovery",
            "MB2: implausible info pointer {:#x}; no RAM regions",
            info_pa
        );
        return 0;
    }
    let total_size = read_u32(info_pa) as usize;
    if !(8..=MB2_MAX_TOTAL_SIZE).contains(&total_size) {
        kernel::log!(
            "discovery",
            "MB2: implausible total_size {:#x}; no RAM regions",
            total_size
        );
        return 0;
    }
    let Some(end) = info_pa.checked_add(total_size) else {
        kernel::log!("discovery", "MB2: info block address wraps; no RAM regions");
        return 0;
    };
    excluded.exclude(info_pa, total_size);
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
                let entries = (tag_size - 16) / entry_size;
                let mut index = 0;
                while index < entries {
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
                    if read_u32(entry + 16) == MEMORY_AVAILABLE
                        && emit_region(sink, read_u64(entry), read_u64(entry + 8))
                    {
                        count += 1;
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
    if count == 0 && basic_mem_upper_kib > 0 {
        if emit_region(sink, 0x10_0000, basic_mem_upper_kib as u64 * 1024) {
            count += 1;
        }
    }
    count
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
fn pvh_regions(
    info_pa: usize,
    sink: &mut dyn FnMut(MemoryRegion),
    excluded: &mut ExclusionTable,
) -> usize {
    let mut count = 0usize;
    let version = read_u32(info_pa + 4);
    if version < 1 {
        kernel::log!(
            "discovery",
            "PVH: unsupported start_info version {}; no RAM regions",
            version
        );
        return 0;
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
        return 0;
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
        return 0;
    };
    excluded.exclude(info_pa, HVM_START_INFO_SIZE);
    excluded.exclude(memmap_pa, memmap_end - memmap_pa);
    for index in 0..entries {
        // `index * 24` cannot overflow: `entries <= 4096` and `memmap_end`
        // above already proved `memmap_pa + entries * 24` fits.
        let entry = memmap_pa + index * PVH_MEMMAP_ENTRY_SIZE;
        if entry + PVH_MEMMAP_ENTRY_SIZE > memmap_end {
            break;
        }
        if read_u32(entry + 16) == MEMORY_AVAILABLE
            && emit_region(sink, read_u64(entry), read_u64(entry + 8))
        {
            count += 1;
        }
    }
    count
}

/// Validate one RSDP and return its retained length (20 / 36): signature,
/// revision-appropriate length and checksums, plus the retained-extent address
/// arithmetic.  `readable` is the number of bytes the caller vouches are
/// readable (already bounded by the source's own extent).
///
/// - revision 0: 20-byte structure, first-20-byte checksum must be zero;
/// - revision >= 2: the `length` field must be exactly 36, and **both** the
///   first-20-byte and the extended (all 36 bytes) checksums must be zero;
/// - any other revision / shape: not retained (the caller publishes `Static` --
///   not "corrupt but continue").
///
/// **Only the RSDP itself is certified**: RSDT / XSDT and their downstream
/// tables are validated by future consumers when they read them.
fn validate_rsdp(pa: usize, readable: usize) -> Option<usize> {
    if pa == 0 || readable < RSDP_V1_SIZE {
        return None;
    }
    // Address arithmetic: the retained extent must be expressible.
    pa.checked_add(readable)?;
    for (offset, byte) in RSDP_SIGNATURE.iter().enumerate() {
        if read_u8(pa + offset) != *byte {
            return None;
        }
    }
    let checksum_zero = |len: usize| {
        let mut sum = 0u8;
        for offset in 0..len {
            sum = sum.wrapping_add(read_u8(pa + offset));
        }
        sum == 0
    };
    // Revision sits after the first checksum (offset 8) and a 6-byte OEM id.
    let revision = read_u8(pa + 15);
    if revision == 0 {
        return checksum_zero(RSDP_V1_SIZE).then_some(RSDP_V1_SIZE);
    }
    // Revision >= 2: the only defined shape is the 36-byte v2 structure, which
    // self-describes its length (a corrupt length must not authorize a larger
    // checksum read).
    if readable < RSDP_V2_SIZE || read_u32(pa + 20) as usize != RSDP_V2_SIZE {
        return None;
    }
    if !checksum_zero(RSDP_V1_SIZE) || !checksum_zero(RSDP_V2_SIZE) {
        return None;
    }
    Some(RSDP_V2_SIZE)
}

/// Find the ACPI RSDP tag in a Multiboot2 information block (tag 15 = new /
/// v2+ preferred, tag 14 = old / v1 fallback), validate the embedded copy, and
/// return `(copy address, retained length)`.
///
/// Allocation-free, bounded by the info block's own `total_size`; the tag scan
/// mirrors [`multiboot2_regions`] (a truncated tag is not trusted).
fn multiboot2_rsdp(info_pa: usize) -> Option<(usize, usize)> {
    if info_pa == 0 || !info_pa.is_multiple_of(8) || info_pa >= BOOT_INFO_MAX_PA {
        return None;
    }
    let total_size = read_u32(info_pa) as usize;
    if !(8..=MB2_MAX_TOTAL_SIZE).contains(&total_size) {
        return None;
    }
    let end = info_pa.checked_add(total_size)?;
    let mut cursor = info_pa + 8;
    let mut old = None;
    let mut new = None;
    while cursor + 8 <= end {
        let tag_type = read_u32(cursor);
        let tag_size = read_u32(cursor + 4) as usize;
        if tag_type == 0 || tag_size < 8 {
            break;
        }
        let tag_end = match cursor.checked_add(tag_size) {
            Some(tail) if tail <= end => tail,
            _ => break,
        };
        if tag_type == MB2_TAG_ACPI_OLD_RSDP || tag_type == MB2_TAG_ACPI_NEW_RSDP {
            let payload = cursor + 8;
            if let Some(length) = validate_rsdp(payload, tag_end - payload) {
                let found = (payload, length);
                if tag_type == MB2_TAG_ACPI_NEW_RSDP {
                    new = new.or(Some(found));
                } else {
                    old = old.or(Some(found));
                }
            }
        }
        cursor = match tag_end.checked_add(7) {
            Some(next) => next & !7,
            None => break,
        };
    }
    new.or(old)
}

/// Discover the retained firmware source (allocation-free raw reads):
/// PVH's `rsdp_paddr`, or the MB2 ACPI RSDP tag copy, each validated before it
/// is published.  The second tuple item is the retained extent that the early
/// arena must exclude **before** any memory is admitted.
///
/// No candidate / a rejected candidate -> [`FirmwareInfo::Static`].  The MB2
/// copy already lives inside the excluded information block; its extent is
/// still reported (redundant exclusions are harmless).
fn discover_firmware(
    protocol: BootProtocol,
    info_pa: usize,
) -> (FirmwareInfo, Option<(usize, usize)>) {
    match protocol {
        BootProtocol::Pvh => {
            if read_u32(info_pa + 4) < 1 {
                return (FirmwareInfo::Static, None);
            }
            let rsdp = read_u64(info_pa + 32) as usize;
            if rsdp == 0 || rsdp >= BOOT_INFO_MAX_PA {
                return (FirmwareInfo::Static, None);
            }
            match validate_rsdp(rsdp, BOOT_INFO_MAX_PA - rsdp) {
                Some(len) => {
                    kernel::log!(
                        "discovery",
                        "PVH: RSDP at {:#x} validated ({} bytes)",
                        rsdp,
                        len
                    );
                    (FirmwareInfo::Acpi { rsdp }, Some((rsdp, len)))
                }
                None => {
                    kernel::log!(
                        "discovery",
                        "PVH: rsdp_paddr {:#x} rejected; firmware: static",
                        rsdp
                    );
                    (FirmwareInfo::Static, None)
                }
            }
        }
        BootProtocol::Multiboot2 => match multiboot2_rsdp(info_pa) {
            Some((rsdp, len)) => {
                kernel::log!(
                    "discovery",
                    "MB2: ACPI RSDP tag at {:#x} validated ({} bytes)",
                    rsdp,
                    len
                );
                (FirmwareInfo::Acpi { rsdp }, Some((rsdp, len)))
            }
            None => (FirmwareInfo::Static, None),
        },
    }
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

/// The discovered 16550 UART as a single-`Pio`-window device descriptor.
///
/// Firmware resource: ISA IRQ 4 (`COM1`).  The logical line stays `None`: this
/// phase has no PIC/IOAPIC routing backend, so the specifier is retained but
/// Core has no deliverable external IRQ for it.
fn uart_device() -> DeviceDescriptor {
    let mut device = DeviceDescriptor::empty();
    device.spaces = vec![IoSpace::Pio {
        base: arch::x86_64::console::COM1 as usize,
        size: 8,
    }]
    .into_boxed_slice();
    device.interrupts = vec![InterruptResource {
        specifier: InterruptSpecifier::Isa { line: 4 },
        line: None,
    }]
    .into_boxed_slice();
    device.compatibles = vec![alloc::boxed::Box::<str>::from("ns16550a")].into_boxed_slice();
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

    let image_start = core::ptr::addr_of!(__kernel_start) as usize;
    let image_end = core::ptr::addr_of!(__kernel_end) as usize;
    let image_size = image_end - image_start;
    let image = MemoryRegion {
        base: image_start,
        size: image_size,
    };

    let protocol = detect_protocol(magic, info_pa);

    // 无堆 pass：在 `early_init` 之前**不分配**——只扫出"包含镜像的 bank"与
    // boot payload 排除区间；完整 RAM inventory 在 seam 之后用 `Vec` 重建。
    let mut exclusions = ExclusionTable::empty();
    // 保留的固件源必须在**归还内存之前**确定：RSDP 本体的保留区间先记进排除
    // 表（`select_arena` / `early_init` 之前），arena 永远不会覆盖它。
    // 无候选 / 校验失败 → `Static`（本机没有保留的受支持固件描述）。
    let (firmware, retained_firmware) = discover_firmware(protocol, info_pa);
    if let Some((base, size)) = retained_firmware {
        exclusions.exclude(base, size);
    }
    let mut bank = None;
    let mut region_count = 0usize;
    walk_usable_regions(
        protocol,
        info_pa,
        &mut |region| {
            region_count += 1;
            if bank.is_none()
                && region.base <= image_start
                && region
                    .base
                    .checked_add(region.size)
                    .is_some_and(|limit| image_end <= limit)
            {
                bank = Some(region);
            }
        },
        &mut exclusions,
    );
    if region_count == 0 {
        panic!("boot discovery: no usable RAM region in boot info");
    }
    let Some(bank) = bank else {
        panic!(
            "boot discovery: no RAM region covers the kernel image {:#x}-{:#x}",
            image_start, image_end
        );
    };

    // Early-memory seam（无堆）：在包含镜像的那个 bank 里，排除镜像与 boot
    // payload（PVH start_info / memmap 或整个 MB2 信息块，加保留的 RSDP），
    // 选出最大的页对齐连续间隙作为 arena。发现的 RAM inventory 是机器真相，
    // **不是** arena；ACPI reclaim / NVS 区间不在 type-1 表内，天然不进 arena。
    let arena = kernel::memory::select_arena(bank, image, |emit| {
        emit(image)?;
        for excluded in &exclusions.exclusions[..exclusions.count] {
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
        region_count
    );
    kernel::log!(
        "bootstrap",
        "early arena {:#x}-{:#x} ({} KiB), boot payload exclusions {}",
        arena.base,
        arena.base + arena.size,
        arena.size / 1024,
        exclusions.count
    );

    // SAFETY: arena 是 boot 从可用 RAM 中选出的连续窗口——镜像（含 boot 栈 /
    // 页表 / 静态缓冲）与全部 boot payload 都已被排除；boot 单 CPU、中断未开、
    // 尚无其它分配者。
    if let Err(error) = unsafe { kernel::memory::early_init(arena) } {
        panic!("early memory init failed: {}", error);
    }

    // seam 之后：完整 inventory（`Vec`，无 boot 侧截断），交接处 boxed。
    let mut memory_regions: Vec<MemoryRegion> = Vec::new();
    walk_usable_regions(
        protocol,
        info_pa,
        &mut |region| memory_regions.push(region),
        &mut ExclusionTable::empty(),
    );

    let cpu_info = vec![CpuInfo {
        boot_cpu: true,
        hardware_id: HardwareCpuId::from_raw(u64::from(boot_apic_id())),
    }];

    let devices = vec![uart_device()];

    let info = MachineInfo {
        boot_hardware_id: cpu_info[0].hardware_id,
        // TSC frequency is not discoverable via CPUID 0x15/0x16 on `qemu64`,
        // so the timebase is **explicitly unknown** (`None`, not fabricated).
        // No timer-derived period is used on this port: `Timer` reports
        // `Unsupported`/`DeliveryUnavailable` and Core polls; a future
        // preempt profile would fail closed on this `None`.
        timebase_frequency: None,
        // 保留的原始固件源（PVH `rsdp_paddr` / MB2 ACPI tag，均先经校验）。
        firmware,
        // AP discovery (ACPI MADT) is not part of this bring-up: the BSP is
        // the only CPU Core may see, and it must be logical CPU0 with
        // `boot_cpu = true`.
        cpu_info: cpu_info.into_boxed_slice(),
        memory_regions: memory_regions.into_boxed_slice(),
        devices: devices.into_boxed_slice(),
    };

    kernel::log!(
        "bootstrap",
        "image {:#x}-{:#x} ({} KiB), {} RAM region(s), boot apic id {}",
        image_start,
        image_end,
        image_size / 1024,
        info.memory_regions.len(),
        info.boot_hardware_id.raw()
    );
    kernel::log!("discovery", "firmware: {:?}", info.firmware);
    kernel::log!("bootstrap", "MachineInfo dump:");
    kernel::printk!("{:#?}\n", info);

    let reserved = [MemoryRegion {
        base: image_start,
        size: image_size,
    }];
    // Core 消费提案并返回唯一提交的 `&'static` 快照：此后 boot 只借用它。
    let info = match kernel::init(info, &reserved) {
        Ok(info) => info,
        Err(error) => panic!("core init failed: {}", error),
    };

    // 保留的 RSDP 必须在早期分配之后仍可按原样重读：重跑完整校验（签名 /
    // 校验和 / 长度）。下游 ACPI 表不在此认证——未来消费者读表时各自校验。
    if let FirmwareInfo::Acpi { rsdp } = info.firmware {
        if validate_rsdp(rsdp, BOOT_INFO_MAX_PA.saturating_sub(rsdp)).is_none() {
            panic!(
                "retained RSDP is unreadable after early allocations (rsdp {:#x})",
                rsdp
            );
        }
        kernel::log!("discovery", "retained RSDP intact at {:#x}", rsdp);
    }
    kernel::log!("bootstrap", "BOOT CORE OK");

    // Local sources are unmasked by `kernel::init`; open the global gate last.
    arch::CpuImpl::enable_irq();

    #[cfg(feature = "selftest")]
    {
        selftest::run(info);
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
