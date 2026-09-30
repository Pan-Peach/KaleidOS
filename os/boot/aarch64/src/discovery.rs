//! FDT discovery → `MachineInfo` normalization for the aarch64 boot.
//!
//! Two responsibilities, both boundary work (bootstrap owns discovery, Core
//! owns truth):
//!
//! 1. **Stage the boot DTB** so the `fdt` parser can consume it.  The staged
//!    blob is a *validated, normalized* copy: the FDT header (magic / version /
//!    totalsize / block offsets and sizes, including the reservation block) is
//!    checked with bounded, checked arithmetic, and an inconsistent header is
//!    rejected (with a log), never "repaired".  QEMU's `arm_load_dtb` sizes the
//!    blob from a fixed 1 MiB buffer, so the blob's `totalsize` overshoots its
//!    real content; and on the `-kernel <elf>` path QEMU 5.2 classifies the
//!    payload as "not Linux" and autoloads the blob at **physical address 0**,
//!    which the parser rejects as a null pointer.  The normalized copy
//!    sidesteps both while keeping every read inside validated bounds.
//! 2. **Normalize** the parsed tree into the owned, fixed-capacity
//!    [`MachineInfo`]: memory regions, `/cpus` (BSP pinned to logical 0), and
//!    root//soc device descriptors.

use alloc::vec::Vec;
use fdt::nodes::AsNode;
use fdt::properties::values::StringList;
use kernel::machine::{
    CompatStr, CpuInfo, DeviceDescriptor, HardwareCpuId, IoSpace, MachineInfo, MemoryRegion,
    MAX_CPUS,
};

/// FDT magic (`0xd00dfeed`, big-endian on the wire).
const FDT_MAGIC: u32 = 0xd00d_feed;

/// Fixed FDT header size (v16/v17): magic, totalsize, three block offsets,
/// version, last_comp_version, boot_cpuid_phys, and two block sizes.
const FDT_HEADER_SIZE: usize = 40;

/// FDT versions this staging accepts (16 and 17 are the flattened-format
/// versions in use; anything else is rejected rather than guessed).
const FDT_VERSION_FIRST: u32 = 16;
const FDT_VERSION_LAST: u32 = 17;

/// Sanity cap for `totalsize`.  QEMU's `arm_load_dtb` writes the blob from a
/// fixed 1 MiB buffer, so its `totalsize` overshoots the real content (observed
/// 0x100000 on QEMU 5.2 `virt`); the normalized copy rewrites it down to the
/// blocks actually copied.
const FDT_MAX_TOTAL_SIZE: usize = 1 << 20;

/// Size of one reservation-map entry (`base: u64, size: u64`).
const FDT_RSV_ENTRY_SIZE: usize = 16;

/// Upper bound for the reservation-block walk: the block ends at a 16-byte zero
/// pair, and an FDT with this many reservations does not exist.
const FDT_MAX_RSV_ENTRIES: usize = 1024;

/// Bounded low-RAM probe window for QEMU's `-kernel <elf>` DTB autoload (the
/// blob is normally at PA 0; this covers a QEMU that moves it into RAM).
const DTB_PROBE_START: usize = 0x4000_0000;
const DTB_PROBE_WINDOW: usize = 1 << 20;

/// Capacity of the staging buffer.  QEMU virt's DTB is a few KiB; 64 KiB leaves
/// generous headroom for a machine with many nodes.
const DTB_COPY_CAPACITY: usize = 64 * 1024;

/// Staging buffer for the boot DTB (8-byte aligned; FDT blocks are 8-aligned).
#[repr(align(8))]
struct DtbCopy([u8; DTB_COPY_CAPACITY]);
static mut DTB_COPY: DtbCopy = DtbCopy([0; DTB_COPY_CAPACITY]);

/// Read a big-endian `u32` from physical address `pa`.
///
/// Boot runs with the MMU off and the identity mapping active, so `pa` is a
/// physical address.  The read is volatile and byte-wise on purpose: the
/// address may be firmware-owned (or, on QEMU's `-kernel <elf>` quirk, physical
/// address 0), so no Rust reference is formed and no alignment is assumed.
/// Callers must bound `pa` to a range the probed header itself vouches for.
fn read_be_u32(pa: usize) -> u32 {
    let mut bytes = [0u8; 4];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        // SAFETY: physical identity-mapped read; see the doc comment.
        *byte = unsafe { core::ptr::read_volatile((pa + offset) as *const u8) };
    }
    u32::from_be_bytes(bytes)
}

/// Read a big-endian `u64` (same raw, bounded convention as [`read_be_u32`]).
fn read_be_u64(pa: usize) -> u64 {
    let mut bytes = [0u8; 8];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        // SAFETY: see [`read_be_u32`].
        *byte = unsafe { core::ptr::read_volatile((pa + offset) as *const u8) };
    }
    u64::from_be_bytes(bytes)
}

/// Exact staged extent of the normalized DTB at `pa`.
///
/// `stage_dtb` rewrites the copy's `totalsize` to the exact prefix it copied,
/// so the staged blob is self-consistent by construction.
pub fn staged_len(pa: usize) -> usize {
    read_be_u32(pa + 4) as usize
}

/// Locate the FDT `/memory` bank that fully covers `[image_start, image_end)`.
///
/// Fails (instead of falling back to an unrelated bank) when no bank contains
/// the loaded image; 64-bit cells that do not fit the target address space are
/// skipped (they cannot contain the image).
pub fn image_bank<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    image_start: usize,
    image_end: usize,
) -> Result<MemoryRegion, &'static str> {
    let Some(memory) = tree.find_node("/memory") else {
        return Err("FDT has no /memory node");
    };
    let Some(regions) = memory.reg() else {
        return Err("FDT /memory has no reg");
    };
    for entry in regions.iter::<u64, u64>() {
        let Ok(entry) = entry else { continue };
        let (Ok(base), Ok(size)) = (usize::try_from(entry.address), usize::try_from(entry.len))
        else {
            continue;
        };
        let Some(end) = base.checked_add(size) else {
            continue;
        };
        if base <= image_start && image_end <= end {
            return Ok(MemoryRegion { base, size });
        }
    }
    Err("no RAM bank contains the loaded image")
}

/// Emit every **FDT-derived** live/reserved interval except the image itself:
/// the retained (staged) FDT extent, its header memory reservation map, and
/// `/reserved-memory` children that carry a fixed `reg`.
///
/// The scan is allocation-free and may be re-run (Core's arena sweep re-reads
/// the firmware records instead of caching them).  An uninterpretable
/// reservation fails closed: it is never silently ignored.
pub fn scan_fdt_exclusions<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    dtb_pa: usize,
    dtb_len: usize,
    emit: &mut dyn FnMut(MemoryRegion) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if dtb_len == 0 {
        return Err("staged FDT extent is zero");
    }
    emit(MemoryRegion {
        base: dtb_pa,
        size: dtb_len,
    })?;

    // Header memory reservation map: 16-byte zero-pair terminated, bounded by
    // the staged extent.
    let mut cursor = tree.header().memory_reserve_map_offset as usize;
    loop {
        let end = cursor
            .checked_add(FDT_RSV_ENTRY_SIZE)
            .ok_or("FDT reservation entry overflows")?;
        if end > dtb_len {
            return Err("FDT reservation block runs past totalsize");
        }
        let base = read_be_u64(dtb_pa + cursor);
        let size = read_be_u64(dtb_pa + cursor + 8);
        if base == 0 && size == 0 {
            break;
        }
        emit_reservation(base, size, emit)?;
        cursor = end;
    }

    // /reserved-memory: only fixed-address (`reg`) children are supported;
    // any other shape could overlap the arena unverified -> fail closed.
    if let Some(node) = tree.find_node("/reserved-memory") {
        for child in node.children() {
            let Some(reg) = child.reg() else {
                return Err("uninterpretable /reserved-memory child (no reg)");
            };
            for entry in reg.iter::<u64, u64>() {
                let entry = entry.map_err(|_| "uninterpretable /reserved-memory reg entry")?;
                emit_reservation(entry.address, entry.len, emit)?;
            }
        }
    }
    Ok(())
}

/// Convert one FDT `(address, size)` reservation to the target address width.
///
/// Zero-size reservations are skipped; an `address` beyond the target width is
/// entirely outside the address space (cannot overlap any arena); a `size` that
/// does not fit fails closed.
fn emit_reservation(
    base: u64,
    size: u64,
    emit: &mut dyn FnMut(MemoryRegion) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    if size == 0 {
        return Ok(());
    }
    let Ok(base) = usize::try_from(base) else {
        return Ok(());
    };
    let size = usize::try_from(size).map_err(|_| "FDT reservation does not fit the target")?;
    emit(MemoryRegion { base, size })
}

/// True when the 16-byte reservation entry at `pa` is the terminating zero pair.
fn reservation_is_terminator(pa: usize) -> bool {
    read_be_u32(pa) == 0
        && read_be_u32(pa + 4) == 0
        && read_be_u32(pa + 8) == 0
        && read_be_u32(pa + 12) == 0
}

/// Walk the reservation block starting at `start` within a `total_size`-byte
/// blob and return the offset just past its terminating zero pair.
fn reservation_end(pa: usize, start: usize, total_size: usize) -> Result<usize, &'static str> {
    let mut entry = start;
    let mut walked = 0;
    while walked < FDT_MAX_RSV_ENTRIES {
        let Some(end) = entry.checked_add(FDT_RSV_ENTRY_SIZE) else {
            return Err("reservation entry overflows");
        };
        if end > total_size {
            return Err("reservation block runs past totalsize");
        }
        if reservation_is_terminator(pa + entry) {
            return Ok(end);
        }
        entry = end;
        walked += 1;
    }
    Err("reservation block unterminated")
}

/// Validate the FDT header at `pa` and return the exact prefix length to copy,
/// or a static reason it is not self-consistent.
///
/// Arithmetic is checked throughout: a corrupt header must neither overflow nor
/// authorize a read outside `totalsize`.  The returned length covers the
/// reservation block as well (walked to its terminator) so the copied blob
/// stays self-consistent, and never exceeds `totalsize`.
fn validated_fdt_len(pa: usize) -> Result<usize, &'static str> {
    let version = read_be_u32(pa + 20);
    let last_comp_version = read_be_u32(pa + 24);
    if !(FDT_VERSION_FIRST..=FDT_VERSION_LAST).contains(&version) {
        return Err("unsupported version");
    }
    if last_comp_version > version {
        return Err("last_comp_version ahead of version");
    }
    let total_size = read_be_u32(pa + 4) as usize;
    if !(FDT_HEADER_SIZE..=FDT_MAX_TOTAL_SIZE).contains(&total_size) {
        return Err("implausible totalsize");
    }
    let structs_start = read_be_u32(pa + 8) as usize;
    let strings_start = read_be_u32(pa + 12) as usize;
    let rsvmap_start = read_be_u32(pa + 16) as usize;
    let structs_size = read_be_u32(pa + 36) as usize;
    let strings_size = read_be_u32(pa + 32) as usize;
    let structs_end = structs_start
        .checked_add(structs_size)
        .ok_or("struct block size overflows")?;
    let strings_end = strings_start
        .checked_add(strings_size)
        .ok_or("strings block size overflows")?;
    if structs_end > total_size || strings_end > total_size {
        return Err("block extends past totalsize");
    }
    let rsvmap_end = reservation_end(pa, rsvmap_start, total_size)?;
    Ok(structs_end
        .max(strings_end)
        .max(rsvmap_end)
        .next_multiple_of(8)
        .min(total_size))
}

/// Copy the validated FDT at physical `pa` into the staging buffer and return
/// its address; `None` if there is no FDT magic there or the header is not
/// self-consistent (version / totalsize / block bounds).
///
/// The copy is *normalized*: `totalsize` is rewritten to the exact prefix that
/// was copied (structs + strings + reservation block, 8-byte aligned), because
/// QEMU's overshoot would otherwise fail the parser's `data.len() >= totalsize`
/// check.  An inconsistent header is rejected and logged -- never repaired.
fn stage_dtb(pa: usize) -> Option<usize> {
    if read_be_u32(pa) != FDT_MAGIC {
        return None;
    }
    let used = match validated_fdt_len(pa) {
        Ok(used) => used,
        Err(reason) => {
            kernel::log!(
                "discovery",
                "DTB candidate at {:#x}: rejected ({})",
                pa,
                reason
            );
            return None;
        }
    };
    if used > DTB_COPY_CAPACITY {
        kernel::log!(
            "discovery",
            "DTB at {:#x}: {:#x} bytes exceeds the {} byte staging buffer",
            pa,
            used,
            DTB_COPY_CAPACITY
        );
        return None;
    }
    // SAFETY: `used` bytes were vouched for by the blob's own header (checked
    // offsets/sizes within `totalsize`), and boot runs identity-mapped with the
    // MMU off, so both addresses are physical; boot is single-threaded and the
    // static outlives the parse.  Byte-wise volatile reads avoid assuming
    // alignment or forming a reference to firmware memory.
    let dst = unsafe { core::ptr::addr_of_mut!(DTB_COPY.0) as *mut u8 };
    let mut index = 0;
    while index < used {
        let byte = unsafe { core::ptr::read_volatile((pa + index) as *const u8) };
        unsafe { core::ptr::write_volatile(dst.add(index), byte) };
        index += 1;
    }
    // Rewrite `totalsize` so the copied prefix is self-consistent.
    unsafe { core::ptr::write_unaligned(dst.add(4) as *mut u32, (used as u32).to_be()) };
    Some(dst as usize)
}

/// Locate and stage the DTB for the `-kernel <elf>` boot.
///
/// The arm64 boot protocol passes the DTB in x0, and QEMU does that for raw
/// Linux Images.  For an **ELF** kernel QEMU 5.2 jumps to the ELF entry without
/// touching the argument registers; its DTB autoload still runs with
/// `dtb_start == 0`, so the blob lands at physical address 0 (observed).
/// Probe x0 first, then PA 0, then a bounded low-RAM window so a QEMU that
/// moves the blob is still covered.  Every candidate must present a fully
/// self-consistent header; nothing is read past the bounds it vouches for.
pub fn stage_boot_dtb(x0: usize) -> Option<usize> {
    if x0 != 0 {
        if let Some(staged) = stage_dtb(x0) {
            return Some(staged);
        }
    }
    if let Some(staged) = stage_dtb(0) {
        return Some(staged);
    }
    let mut offset = 0;
    while offset < DTB_PROBE_WINDOW {
        if let Some(staged) = stage_dtb(DTB_PROBE_START + offset) {
            return Some(staged);
        }
        offset += 8;
    }
    None
}

/// Parser flavour produced by `Fdt::from_ptr_unaligned`.
type FdtParser<'a> = (
    fdt::parsing::unaligned::UnalignedParser<'a>,
    fdt::parsing::Panic,
);

/// Extract one FDT node's device descriptor.
/// Filter: must have `reg` and a non-empty `compatible` (memory/cpus/chosen/pmu
/// fail one of those naturally).
fn device_descriptor<'a>(child: &fdt::nodes::Node<'a, FdtParser<'a>>) -> Option<DeviceDescriptor> {
    let mut descriptor = None;
    if let Some(r) = child.reg() {
        if let Some(reg) = r.iter::<u64, u64>().flatten().next() {
            descriptor = Some(DeviceDescriptor {
                space: IoSpace::Mmio {
                    base: reg.address as usize,
                    size: reg.len as usize,
                },
                irq: None,
                compatibles: [CompatStr::empty(); 4],
                compat_count: 0,
            });
        }
    }
    let mut d = descriptor?;
    if let Some(comp) = child.properties().find("compatible") {
        if let Ok(list) = comp.as_value::<StringList>() {
            for s in list {
                let idx = d.compat_count as usize;
                if idx < d.compatibles.len() {
                    d.compatibles[idx] = CompatStr::from_bytes(s.as_bytes());
                    d.compat_count += 1;
                }
            }
        }
    }
    if d.compat_count == 0 {
        return None;
    }
    if let Some(irqs) = child.properties().find("interrupts") {
        d.irq = irqs.as_value::<u32>().ok();
    }
    Some(d)
}

/// Collect matching device descriptors from a batch of FDT child nodes (owned
/// `Vec`; no capacity limit at discovery).
fn collect_devices<'a>(
    children: impl IntoIterator<Item = fdt::nodes::Node<'a, FdtParser<'a>>>,
    devices: &mut Vec<DeviceDescriptor>,
) {
    for child in children {
        if let Some(d) = device_descriptor(&child) {
            devices.push(d);
        }
    }
}

/// Normalize a parsed DTB into the owned [`MachineInfo`] Core consumes.
///
/// `boot_affinity` is the BSP's MPIDR affinity (the same shape FDT
/// `/cpus/*/reg` uses); the boot CPU is pinned to `cpu_info[0]` because Core's
/// `CpuRegistry::build` requires BSP == logical CPU0.  A BSP missing from
/// `/cpus` is **not** fabricated here: Core rejects the proposal (boot must not
/// manufacture a CPU).  More CPUs than `MAX_CPUS` are truncated with the BSP
/// first and an explicit diagnostic.
pub fn discover<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    boot_affinity: u64,
    timebase_frequency: u64,
) -> MachineInfo {
    let mut memory_regions: Vec<MemoryRegion> = Vec::new();
    for region in tree.root().memory().reg().iter::<u64, u64>() {
        let Ok(r) = region else { continue };
        memory_regions.push(MemoryRegion {
            base: r.address as usize,
            size: r.len as usize,
        });
    }

    let mut cpu_info: Vec<CpuInfo> = Vec::new();
    for cpu in tree.root().cpus().iter() {
        let affinity = cpu.reg::<u64>().first().unwrap_or(0);
        cpu_info.push(CpuInfo {
            boot_cpu: affinity == boot_affinity,
            hardware_id: HardwareCpuId::from_raw(affinity),
        });
    }

    // Core invariant: BSP == logical CPU0.  FDT usually lists cpu@0 first; the
    // swap makes discovery order irrelevant.  If the BSP is missing entirely
    // (affinity mismatch), the proposal is left as discovered — Core rejects it
    // rather than boot fabricating a CPU that firmware never described.
    if let Some(boot_index) = cpu_info.iter().position(|c| c.boot_cpu) {
        cpu_info.swap(0, boot_index);
    }
    if cpu_info.len() > MAX_CPUS {
        kernel::log!(
            "discovery",
            "too many CPUs ({}); admitting {} (BSP first)",
            cpu_info.len(),
            MAX_CPUS
        );
        cpu_info.truncate(MAX_CPUS);
    }

    // Two passes: root holds system-level devices, /soc holds bus devices.
    let mut devices: Vec<DeviceDescriptor> = Vec::new();
    collect_devices(tree.root().as_node().children(), &mut devices);
    if let Some(soc) = tree.find_node("/soc") {
        collect_devices(soc.children(), &mut devices);
    }

    MachineInfo {
        boot_hardware_id: HardwareCpuId::from_raw(boot_affinity),
        timebase_frequency,
        cpu_info: cpu_info.into_boxed_slice(),
        memory_regions: memory_regions.into_boxed_slice(),
        devices: devices.into_boxed_slice(),
    }
}
