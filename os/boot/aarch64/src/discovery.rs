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
//! 2. **Normalize** the parsed tree into the owned (no fixed capacity)
//!    [`MachineInfo`]: memory regions, `/cpus` (BSP pinned to logical 0), and
//!    root//soc device descriptors (all `reg` windows, all compatibles).
//! 3. **Retain** the staged DTB as the raw firmware source: the normalized
//!    [`MachineInfo`] carries [`FirmwareInfo::Fdt`] pointing at the **staged
//!    copy** (with its rewritten validated length) -- never the original PA-0
//!    address -- so a future driver can read vendor data beyond the normalized
//!    view.  The staged bytes are never reclaimed.

use alloc::boxed::Box;
use alloc::vec::Vec;
use fdt::nodes::{AsNode, Node};
use fdt::properties::reg::Reg;
use fdt::properties::values::StringList;
use fdt::properties::PHandle;
use kernel::machine::{
    CpuInfo, DeviceDescriptor, FirmwareInfo, HardwareCpuId, InterruptResource, InterruptSpecifier,
    IoSpace, MachineInfo, MemoryRegion, MAX_CPUS,
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

/// Re-read a retained staged copy: magic and rewritten `totalsize` must be
/// unchanged.  `size` is the retained length boot recorded in
/// [`FirmwareInfo::Fdt`]; this proves the bytes survived the early allocations
/// (the staging buffer lives in the image, which Core marks permanently
/// reserved) and is what a future consumer would read.
pub fn staged_dtb_intact(pa: usize, size: usize) -> bool {
    read_be_u32(pa) == FDT_MAGIC && read_be_u32(pa + 4) as usize == size
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

/// Maximum cells in one interrupt specifier (defends against a malformed
/// `#interrupt-cells`).
const MAX_INTERRUPT_CELLS: usize = 16;

/// Maximum interrupt-parent chain steps (defends against self-referencing or
/// malformed trees).
const MAX_PARENT_STEPS: usize = 16;

/// Collect the device records (root children, then `/soc` children) with their
/// complete interrupt resources.
///
/// AArch64 has no GIC routing in this phase: every retained specifier keeps
/// `line: None` (the logical IRQ is deliberately *not* decoded from the INTID).
pub fn collect_devices<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
) -> Result<Vec<DeviceDescriptor>, &'static str> {
    let mut devices: Vec<DeviceDescriptor> = Vec::new();
    collect(tree, tree.root().as_node().children(), &mut devices)?;
    if let Some(soc) = tree.find_node("/soc") {
        collect(tree, soc.children(), &mut devices)?;
    }
    Ok(devices)
}

fn collect<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    children: impl IntoIterator<Item = Node<'a, FdtParser<'a>>>,
    devices: &mut Vec<DeviceDescriptor>,
) -> Result<(), &'static str> {
    for child in children {
        if let Some(descriptor) = device_descriptor(tree, &child)? {
            devices.push(descriptor);
        }
    }
    Ok(())
}

/// Extract one FDT node's device descriptor.
///
/// Filter: must have a compatible and at least one representable `reg` window
/// (memory/cpus/chosen/pmu fail one of those naturally).  Every supported
/// `reg` entry is collected in firmware order (`spaces[0]` is the primary
/// window); every compatible string is kept (no count/length truncation).
/// Addresses must fit `usize` with a non-overflowing `base + size`.
fn device_descriptor<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    child: &Node<'a, FdtParser<'a>>,
) -> Result<Option<DeviceDescriptor>, &'static str> {
    let mut compatibles: Vec<Box<str>> = Vec::new();
    if let Some(comp) = child.properties().find("compatible") {
        if let Ok(list) = comp.as_value::<StringList>() {
            for value in list {
                compatibles.push(Box::from(value));
            }
        }
    }
    if compatibles.is_empty() {
        return Ok(None);
    }

    let Some(reg) = child.reg() else {
        return Ok(None);
    };
    if !identity_addressable(child) {
        // Never treat a translated child address as a CPU address: diagnose
        // and omit the device instead of inventing an MMIO window.
        kernel::log!(
            "discovery",
            "device requires address translation; omitted (no MMIO address invented)"
        );
        return Ok(None);
    }
    let spaces = collect_spaces(reg);
    if spaces.is_empty() {
        return Ok(None);
    }

    Ok(Some(DeviceDescriptor {
        spaces: spaces.into_boxed_slice(),
        interrupts: interrupts_of(tree, child)?,
        compatibles: compatibles.into_boxed_slice(),
    }))
}

/// Collect every supported `reg` entry (firmware order): u64 → `usize` checked
/// conversion plus `base + size` overflow check; unrepresentable entries are
/// diagnosed and skipped, never truncated into a bogus address.
fn collect_spaces(reg: Reg<'_>) -> Vec<IoSpace> {
    let mut spaces = Vec::new();
    for entry in reg.iter::<u64, u64>() {
        let Ok(entry) = entry else {
            kernel::log!("discovery", "malformed reg entry; skipped");
            continue;
        };
        let (Ok(base), Ok(size)) = (usize::try_from(entry.address), usize::try_from(entry.len))
        else {
            kernel::log!(
                "discovery",
                "reg window {:#x}+{:#x} does not fit usize; skipped",
                entry.address,
                entry.len
            );
            continue;
        };
        if base.checked_add(size).is_none() {
            kernel::log!(
                "discovery",
                "reg window {base:#x}+{size:#x} overflows; skipped"
            );
            continue;
        }
        spaces.push(IoSpace::Mmio { base, size });
    }
    spaces
}

/// Whether the device lives on an identity address chain:
/// - a direct child of the root node is in the firmware CPU address space;
/// - otherwise every ancestor bus must declare an **empty** `ranges` (the
///   devicetree spec: empty ranges = child and parent address spaces are
///   identical).  A missing `ranges` means no mapping exists; a non-empty
///   `ranges` needs translation -- neither is supported in this phase.
fn identity_addressable<'a>(node: &Node<'a, FdtParser<'a>>) -> bool {
    let mut current = *node;
    while let Some(parent) = current.parent() {
        if parent.parent().is_none() {
            return true; // direct root child: child addresses are CPU addresses
        }
        match parent.properties().find("ranges") {
            Some(property) if property.value.is_empty() => {}
            _ => return false,
        }
        current = parent;
    }
    false
}

/// Parse the node's complete interrupt resources (`interrupts-extended` wins
/// over `interrupts`, matching the devicetree spec / the `fdt` crate); both
/// missing is legal and yields an empty list.
fn interrupts_of<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    node: &Node<'a, FdtParser<'a>>,
) -> Result<Box<[InterruptResource]>, &'static str> {
    if let Some(property) = node.properties().find("interrupts-extended") {
        return parse_extended(tree, property.value);
    }
    if let Some(property) = node.properties().find("interrupts") {
        return parse_legacy(tree, node, property.value);
    }
    Ok(Box::new([]))
}

/// `interrupts-extended`: each tuple is `phandle + that controller's
/// #interrupt-cells` cells.  Truncation / unresolved phandles are discovery
/// errors.
fn parse_extended<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    mut rest: &[u8],
) -> Result<Box<[InterruptResource]>, &'static str> {
    let mut resources = Vec::new();
    while !rest.is_empty() {
        let Some((phandle_bytes, tail)) = rest.split_at_checked(4) else {
            return Err("interrupts-extended: truncated phandle");
        };
        let phandle = u32::from_be_bytes(phandle_bytes.try_into().expect("4 bytes"));
        rest = tail;
        let controller = resolve_controller(tree, phandle)?;
        let cells_count = interrupt_cells(&controller)?;
        let bytes = cells_count * 4;
        let Some((cells_bytes, tail)) = rest.split_at_checked(bytes) else {
            return Err("interrupts-extended: truncated specifier");
        };
        rest = tail;
        resources.push(InterruptResource {
            specifier: InterruptSpecifier::Fdt {
                controller: phandle,
                cells: collect_cells(cells_bytes),
            },
            line: None,
        });
    }
    Ok(resources.into_boxed_slice())
}

/// `interrupts`: every tuple shares the (resolved, inherited) interrupt parent;
/// the length must be a whole multiple of its `#interrupt-cells`.
fn parse_legacy<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    node: &Node<'a, FdtParser<'a>>,
    bytes: &[u8],
) -> Result<Box<[InterruptResource]>, &'static str> {
    if bytes.is_empty() {
        return Ok(Box::new([]));
    }
    let controller = interrupt_parent(tree, node)?;
    let cells_count = interrupt_cells(&controller)?;
    let stride = cells_count * 4;
    if !bytes.len().is_multiple_of(stride) {
        return Err("interrupts: length is not a multiple of #interrupt-cells");
    }
    let phandle = controller
        .property::<PHandle>()
        .map(PHandle::as_u32)
        .ok_or("interrupt controller node has no phandle")?;
    let mut resources = Vec::new();
    for cells_bytes in bytes.chunks_exact(stride) {
        resources.push(InterruptResource {
            specifier: InterruptSpecifier::Fdt {
                controller: phandle,
                cells: collect_cells(cells_bytes),
            },
            line: None,
        });
    }
    Ok(resources.into_boxed_slice())
}

fn collect_cells(bytes: &[u8]) -> Box<[u32]> {
    bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_be_bytes(chunk.try_into().expect("chunk is 4 bytes")))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Resolve the interrupt parent (Linux `of_irq_find_parent`): follow the node's
/// own `interrupt-parent`; otherwise climb the devicetree parent; stop at the
/// first node carrying `#interrupt-cells`.  Unresolved phandles / too-deep
/// chains are discovery errors.
fn interrupt_parent<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    node: &Node<'a, FdtParser<'a>>,
) -> Result<Node<'a, FdtParser<'a>>, &'static str> {
    let mut current = *node;
    for _ in 0..MAX_PARENT_STEPS {
        let next = if let Some(property) = current.properties().find("interrupt-parent") {
            let phandle = property
                .as_value::<u32>()
                .map_err(|_| "invalid interrupt-parent")?;
            resolve_controller(tree, phandle)?
        } else {
            current.parent().ok_or("device has no interrupt parent")?
        };
        if next.properties().find("#interrupt-cells").is_some() {
            return Ok(next);
        }
        current = next;
    }
    Err("interrupt-parent chain is too deep")
}

fn resolve_controller<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    phandle: u32,
) -> Result<Node<'a, FdtParser<'a>>, &'static str> {
    tree.root()
        .resolve_phandle(PHandle::new(phandle))
        .ok_or("interrupt controller phandle does not resolve")
}

fn interrupt_cells<'a>(controller: &Node<'a, FdtParser<'a>>) -> Result<usize, &'static str> {
    let property = controller
        .properties()
        .find("#interrupt-cells")
        .ok_or("interrupt controller has no #interrupt-cells")?;
    let count = property
        .as_value::<u32>()
        .map_err(|_| "invalid #interrupt-cells")? as usize;
    if count == 0 || count > MAX_INTERRUPT_CELLS {
        return Err("#interrupt-cells out of range");
    }
    Ok(count)
}

/// Normalize a parsed DTB into the owned [`MachineInfo`] Core consumes.
///
/// `boot_affinity` is the BSP's MPIDR affinity (the same shape FDT
/// `/cpus/*/reg` uses); the boot CPU is pinned to `cpu_info[0]` because Core's
/// `CpuRegistry::build` requires BSP == logical CPU0.  A BSP missing from
/// `/cpus` is **not** fabricated here: Core rejects the proposal (boot must not
/// manufacture a CPU).  More CPUs than `MAX_CPUS` are truncated with the BSP
/// first and an explicit diagnostic.
///
/// `firmware` is the retained raw source ([`FirmwareInfo::Fdt`] pointing at the
/// staged copy); it is stored verbatim -- this function does not re-validate it.
///
/// Malformed / unresolved interrupt tuples are a hard discovery error (boot
/// fails with the reason instead of silently dropping resources).
pub fn discover<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    boot_affinity: u64,
    timebase_frequency: u64,
    firmware: FirmwareInfo,
) -> Result<MachineInfo, &'static str> {
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
    let devices = collect_devices(tree)?;

    Ok(MachineInfo {
        boot_hardware_id: HardwareCpuId::from_raw(boot_affinity),
        timebase_frequency,
        firmware,
        cpu_info: cpu_info.into_boxed_slice(),
        memory_regions: memory_regions.into_boxed_slice(),
        devices: devices.into_boxed_slice(),
    })
}
