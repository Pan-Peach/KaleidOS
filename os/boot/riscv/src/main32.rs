//! RV32 bare bootstrap for the QEMU/OpenSBI-style hand-off.
//!
//! This profile keeps the image in an identity-mapped Sv32 view.  It exercises
//! the RV32 CPU/firmware path without assuming that every RV32 machine has the
//! same high-half layout; the reusable address-space backend remains below
//! `arch::riscv::mmu`.

use arch::CpuArch;
use core::arch::global_asm;
use core::panic::PanicInfo;
use fdt::nodes::AsNode;
use fdt::properties::values::StringList;
use kernel::machine::{
    CompatStr, CpuId, CpuInfo, DeviceDescriptor, IoSpace, MachineInfo, MemoryRegion,
};

#[path = "console.rs"]
mod console;

global_asm!(include_str!("entry32.S"));

unsafe extern "C" {
    static __image_load_start: u8;
    static __image_end: u8;
}

#[used]
#[unsafe(link_section = ".initpkg")]
static INITPKG: [u8; include_bytes!("../../../../tools/qemu/init.kpkg").len()] =
    *include_bytes!("../../../../tools/qemu/init.kpkg");

type FdtParser<'a> = (
    fdt::parsing::unaligned::UnalignedParser<'a>,
    fdt::parsing::Panic,
);

fn linker_addr(symbol: *const u8) -> usize {
    symbol as usize
}

fn discover(dtb_pa: usize, hart_id: usize) -> Result<MachineInfo, ()> {
    let tree = unsafe { fdt::Fdt::from_ptr_unaligned(dtb_pa as *const u8) }.map_err(|_| ())?;
    let mut memory_regions = [MemoryRegion { base: 0, size: 0 }; 16];
    let mut cpu_info = [CpuInfo {
        boot_cpu: false,
        hart_id: CpuId(0),
    }; 8];
    let mut devices = [DeviceDescriptor::empty(); 26];

    let mut mem_count = 0;
    for region in tree.root().memory().reg().iter::<u64, u64>() {
        let Ok(region) = region else { continue };
        // FDT 描述是 u64；32 位目标只能表达 [0, 2^32) 的区间。
        // 越界区间（如 -m 4G 的 0x80000000+0x100000000）直接丢弃，
        // 不能 `as usize` 截断——否则会回绕出虚假区间，core::init 帧区失败。
        let Some(end) = region.address.checked_add(region.len) else {
            continue;
        };
        if end > u32::MAX as u64 || region.address > u32::MAX as u64 {
            continue;
        }
        if mem_count < memory_regions.len() {
            memory_regions[mem_count] = MemoryRegion {
                base: region.address as usize,
                size: region.len as usize,
            };
            mem_count += 1;
        }
    }

    let mut cpu_count = 0;
    for cpu in tree.root().cpus().iter() {
        if cpu_count < cpu_info.len() {
            let hart = cpu.reg::<u64>().first().unwrap_or(0);
            cpu_info[cpu_count] = CpuInfo {
                boot_cpu: hart == hart_id as u64,
                hart_id: CpuId(hart as usize),
            };
            cpu_count += 1;
        }
    }

    let mut dev_count = 0;
    collect_devices(
        tree.root().as_node().children(),
        &mut devices,
        &mut dev_count,
    );
    if let Some(soc) = tree.find_node("/soc") {
        collect_devices(soc.children(), &mut devices, &mut dev_count);
    }

    Ok(MachineInfo {
        boot_hart: hart_id,
        cpu_count,
        cpu_info,
        mem_count,
        memory_regions,
        dev_count,
        devices,
    })
}

/// OpenSBI 选定的 boot hart 进入 payload；RV32 profile 使用 identity Sv32。
#[unsafe(no_mangle)]
extern "C" fn bootstrap_main(hart_id: usize, dtb_pa: usize, kernel_pa: usize) -> ! {
    arch::CpuImpl::init();
    kernel::log!("bootstrap", "KaleidOS RV32 bootstrap");

    let info = match discover(dtb_pa, hart_id) {
        Ok(info) if info.mem_count != 0 => info,
        Ok(_) => panic!("RV32 boot failed: no RAM region"),
        Err(_) => panic!("FDT magic: BAD"),
    };

    let linked_start =
        arch::physical_address_of(linker_addr(core::ptr::addr_of!(__image_load_start)));
    let linked_end = arch::physical_address_of(linker_addr(core::ptr::addr_of!(__image_end)));
    let image_size = linked_end
        .checked_sub(linked_start)
        .expect("invalid RV32 image range");
    let reserved = [MemoryRegion {
        base: kernel_pa,
        size: image_size,
    }];

    match kernel::init(&info, &reserved) {
        Ok(()) => {
            kernel::log!("bootstrap", "RV32 CORE OK");
            let pkg_start = core::ptr::addr_of!(INITPKG) as usize;
            let pkg = unsafe { core::slice::from_raw_parts(pkg_start as *const u8, INITPKG.len()) };
            kernel::component::store::init(pkg);
            kernel::monitor::run();
        }
        Err(error) => panic!("core init failed: {}", error),
    }
}

fn device_descriptor<'a>(child: &fdt::nodes::Node<'a, FdtParser<'a>>) -> Option<DeviceDescriptor> {
    let mut descriptor = None;
    if let Some(regs) = child.reg() {
        for entry in regs.iter::<u64, u64>() {
            if let Ok(reg) = entry {
                descriptor = Some(DeviceDescriptor {
                    space: IoSpace::Mmio {
                        base: reg.address as usize,
                        size: reg.len as usize,
                    },
                    irq: None,
                    compatibles: [CompatStr::empty(); 4],
                    compat_count: 0,
                });
                break;
            }
        }
    }
    let mut descriptor = descriptor?;
    if let Some(property) = child.properties().find("compatible") {
        if let Ok(list) = property.as_value::<StringList>() {
            for value in list {
                let index = descriptor.compat_count as usize;
                if index < descriptor.compatibles.len() {
                    descriptor.compatibles[index] = CompatStr::from_bytes(value.as_bytes());
                    descriptor.compat_count += 1;
                }
            }
        }
    }
    if descriptor.compat_count == 0 {
        return None;
    }
    if let Some(irqs) = child.properties().find("interrupts") {
        descriptor.irq = irqs.as_value::<u32>().ok();
    }
    Some(descriptor)
}

fn collect_devices<'a>(
    children: impl IntoIterator<Item = fdt::nodes::Node<'a, FdtParser<'a>>>,
    devices: &mut [DeviceDescriptor; 26],
    dev_count: &mut usize,
) {
    for child in children {
        if let Some(device) = device_descriptor(&child) {
            if *dev_count < devices.len() {
                devices[*dev_count] = device;
                *dev_count += 1;
            }
        }
    }
}

struct DirectWriter;

impl core::fmt::Write for DirectWriter {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        for byte in value.bytes() {
            console::write_byte(byte);
        }
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let mut writer = DirectWriter;
    let _ = core::fmt::write(&mut writer, format_args!("\nPANIC: {}\n", info));
    loop {
        core::hint::spin_loop();
    }
}
