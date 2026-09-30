//! FDT discovery → `MachineInfo` normalization for the aarch64 boot.
//!
//! Two responsibilities, both boundary work (bootstrap owns discovery, Core
//! owns truth):
//!
//! 1. **Stage the boot DTB** so the `fdt` parser can consume it.  QEMU's
//!    `arm_load_dtb` sizes the blob from a fixed 1 MiB buffer, so the blob's
//!    `totalsize` overshoots its real content; and on the `-kernel <elf>` path
//!    QEMU 5.2 classifies the payload as "not Linux" and autoloads the blob at
//!    **physical address 0**, which the parser rejects as a null pointer.
//! 2. **Normalize** the parsed tree into the owned, fixed-capacity
//!    [`MachineInfo`]: memory regions, `/cpus` (BSP pinned to logical 0), and
//!    root//soc device descriptors.

use fdt::nodes::AsNode;
use fdt::properties::values::StringList;
use kernel::machine::{
    CompatStr, CpuInfo, DeviceDescriptor, HardwareCpuId, IoSpace, MachineInfo, MemoryRegion,
    MAX_CPUS,
};

/// FDT magic (`0xd00dfeed`, big-endian on the wire).
const FDT_MAGIC: u32 = 0xd00d_feed;

/// Capacity of the staging buffer.  QEMU virt's DTB is a few KiB; 64 KiB leaves
/// generous headroom for a machine with many nodes.
const DTB_COPY_CAPACITY: usize = 64 * 1024;

/// Staging buffer for the boot DTB (8-byte aligned; FDT blocks are 8-aligned).
#[repr(align(8))]
struct DtbCopy([u8; DTB_COPY_CAPACITY]);
static mut DTB_COPY: DtbCopy = DtbCopy([0; DTB_COPY_CAPACITY]);

/// Big-endian `u32` read from a physical address (MMU off).
fn read_be_u32(pa: usize) -> u32 {
    // SAFETY: caller passes addresses it knows are mapped (x0 / PA 0 / low RAM).
    unsafe { u32::from_be(core::ptr::read_unaligned(pa as *const u32)) }
}

/// Copy a big-endian FDT at `pa` into the staging buffer and return its
/// address; `None` if there is no FDT magic there or it does not fit.
///
/// `totalsize` is rewritten to the actually-used prefix
/// (`max(off_dt_struct + size_dt_struct, off_dt_strings + size_dt_strings)`,
/// 8-byte aligned), because QEMU's overshoot would otherwise fail the parser's
/// `data.len() >= totalsize` check.
fn stage_dtb(pa: usize) -> Option<usize> {
    if read_be_u32(pa) != FDT_MAGIC {
        return None;
    }
    let structs_start = read_be_u32(pa + 8) as usize;
    let strings_start = read_be_u32(pa + 12) as usize;
    let strings_size = read_be_u32(pa + 32) as usize;
    let structs_size = read_be_u32(pa + 36) as usize;
    let used = (structs_start + structs_size)
        .max(strings_start + strings_size)
        .max(40)
        .next_multiple_of(8);
    if used > DTB_COPY_CAPACITY {
        return None;
    }
    // SAFETY: `used` bytes are readable at `pa` (verified FDT magic + header
    // fields) and `dst` is a static buffer; boot is single-threaded.
    let dst = unsafe { core::ptr::addr_of_mut!(DTB_COPY.0) as *mut u8 };
    unsafe {
        core::ptr::copy_nonoverlapping(pa as *const u8, dst, used);
        core::ptr::write_unaligned(dst.add(4) as *mut u32, (used as u32).to_be());
    }
    Some(dst as usize)
}

/// Locate and stage the DTB for the `-kernel <elf>` boot.
///
/// The arm64 boot protocol passes the DTB in x0, and QEMU does that for raw
/// Linux Images.  For an **ELF** kernel QEMU 5.2 jumps to the ELF entry without
/// touching the argument registers; its DTB autoload still runs with
/// `dtb_start == 0`, i.e. the blob lands at physical address 0.  Probe x0
/// first, then PA 0, then a bounded low-RAM window so a QEMU that moves the
/// blob is still covered.
pub fn stage_boot_dtb(x0: usize) -> Option<usize> {
    if x0 != 0 {
        if let Some(staged) = stage_dtb(x0) {
            return Some(staged);
        }
    }
    if let Some(staged) = stage_dtb(0) {
        return Some(staged);
    }
    let start = 0x4000_0000usize;
    let mut pa = start;
    while pa < start + (1 << 20) {
        if let Some(staged) = stage_dtb(pa) {
            return Some(staged);
        }
        pa += 8;
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

/// Collect matching device descriptors from a batch of FDT child nodes into
/// the fixed-capacity `MachineInfo` table.
fn collect_devices<'a>(
    children: impl IntoIterator<Item = fdt::nodes::Node<'a, FdtParser<'a>>>,
    devices: &mut [DeviceDescriptor; 26],
    dev_count: &mut usize,
) {
    for child in children {
        if let Some(d) = device_descriptor(&child) {
            if *dev_count < devices.len() {
                devices[*dev_count] = d;
                *dev_count += 1;
            }
        }
    }
}

/// Normalize a parsed DTB into the owned [`MachineInfo`] Core consumes.
///
/// `boot_affinity` is the BSP's MPIDR affinity (the same shape FDT
/// `/cpus/*/reg` uses); the boot CPU is pinned to `cpu_info[0]` because Core's
/// `CpuRegistry::build` requires BSP == logical CPU0.
pub fn discover<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    boot_affinity: u64,
    timebase_frequency: u64,
) -> MachineInfo {
    let mut memory_regions = [MemoryRegion { base: 0, size: 0 }; 16];
    let mut cpu_info = [CpuInfo {
        boot_cpu: false,
        hardware_id: HardwareCpuId::from_raw(0),
    }; MAX_CPUS];
    let mut devices = [DeviceDescriptor::empty(); 26];

    let mut mem_count = 0usize;
    for region in tree.root().memory().reg().iter::<u64, u64>() {
        let Ok(r) = region else { continue };
        if mem_count >= memory_regions.len() {
            kernel::log!("discovery", "too many RAM regions; dropping");
            continue;
        }
        memory_regions[mem_count] = MemoryRegion {
            base: r.address as usize,
            size: r.len as usize,
        };
        mem_count += 1;
    }

    let mut cpu_count = 0usize;
    for cpu in tree.root().cpus().iter() {
        if cpu_count >= cpu_info.len() {
            kernel::log!("discovery", "too many CPUs; dropping");
            continue;
        }
        let affinity = cpu.reg::<u64>().first().unwrap_or(0);
        cpu_info[cpu_count] = CpuInfo {
            boot_cpu: affinity == boot_affinity,
            hardware_id: HardwareCpuId::from_raw(affinity),
        };
        cpu_count += 1;
    }

    // Core invariant: BSP == logical CPU0.  FDT usually lists cpu@0 first; the
    // swap makes discovery order irrelevant.  If the BSP is missing entirely
    // (affinity mismatch), pin index 0 to it so the topology stays bootable —
    // the log records the disagreement.
    if let Some(boot_index) = cpu_info[..cpu_count].iter().position(|c| c.boot_cpu) {
        cpu_info.swap(0, boot_index);
    } else if cpu_count > 0 {
        kernel::log!(
            "discovery",
            "boot affinity {:#x} not in /cpus; pinning cpu_info[0]",
            boot_affinity
        );
        cpu_info[0] = CpuInfo {
            boot_cpu: true,
            hardware_id: HardwareCpuId::from_raw(boot_affinity),
        };
    }

    // Two passes: root holds system-level devices, /soc holds bus devices.
    let mut dev_count = 0usize;
    collect_devices(
        tree.root().as_node().children(),
        &mut devices,
        &mut dev_count,
    );
    if let Some(soc) = tree.find_node("/soc") {
        collect_devices(soc.children(), &mut devices, &mut dev_count);
    }

    MachineInfo {
        boot_hardware_id: HardwareCpuId::from_raw(boot_affinity),
        timebase_frequency,
        cpu_count,
        cpu_info,
        mem_count,
        memory_regions,
        dev_count,
        devices,
    }
}
