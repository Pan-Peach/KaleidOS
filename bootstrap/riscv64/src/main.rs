#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;
use fdt::properties::values::StringList;
use kernel::machine::{CompatStr, CpuInfo, DeviceDescriptor, MachineInfo, MemoryRegion};

mod console;
mod sbi;

global_asm!(include_str!("entry.S"));

// 链接脚本符号：本文档镜像（bootstrap + core 单一 kaleidos.elf）的物理范围。
// 取地址（不是值）：这段是 Core 自己，启动后永久 Reserved。
unsafe extern "C" {
    static __bootstrap_start: u8;
    static __bootstrap_end: u8;
}

/// 只允许 boot hart 继续启动；其余 hart 全部 park（OpenSBI 会把 domain 内所有 hart 都跳进来）。
#[unsafe(no_mangle)]
extern "C" fn bootstrap_main(hart_id: usize, dtb_pa: usize) -> ! {
    console::log("bootstrap", "KaleidOS bootstrap\n");
    console::log("bootstrap", "========================================\n");

    // FDT 发现：直接吃 OpenSBI 给的 dtb 物理地址（unsafe：该地址有效性 Rust 无从验证）
    match unsafe { fdt::Fdt::from_ptr_unaligned(dtb_pa as *const u8) } {
        Ok(tree) => {
            console::log("bootstrap", "FDT magic: OK\n");

            // 归一化：fdt 类型 → core::machine 类型（owned，DTB 用完可丢）。
            // MachineInfo 是定长数组 + count（无借用），字符串用 CompatStr 内嵌复制。
            let mut memory_regions = [MemoryRegion { base: 0, size: 0 }; 16];
            let mut cpu_info = [CpuInfo { boot_cpu: false, hart_id: 0 }; 8];
            let mut devices = [DeviceDescriptor::empty(); 26];

            let mut mem_count = 0usize;
            for region in tree.root().memory().reg().iter::<u64, u64>() {
                if let Ok(r) = region {
                    memory_regions[mem_count] = MemoryRegion {
                        base: r.address as usize,
                        size: r.len as usize,
                    };
                    mem_count += 1;
                }
            }

            let mut cpu_count = 0usize;
            for cpu in tree.root().cpus().iter() {
                let hart = cpu.reg::<u64>().first().unwrap_or(0);
                cpu_info[cpu_count] = CpuInfo {
                    boot_cpu: hart == hart_id as u64,
                    hart_id: hart as usize,
                };
                cpu_count += 1;
            }

            // 设备清单：遍历 /soc 下带 reg 的子节点（virtio/mmio uart 等）
            // CompatStr 内嵌复制 → 单遍填充，无需字符串池
            let mut dev_count = 0usize;
            if let Some(soc) = tree.find_node("/soc") {
                for child in soc.children() {
                    let mut descriptor = None;
                    if let Some(r) = child.reg() {
                        for entry in r.iter::<u64, u64>() {
                            if let Ok(reg) = entry {
                                descriptor = Some(DeviceDescriptor {
                                    mmio_base: reg.address as usize,
                                    mmio_size: reg.len as usize,
                                    irq: None,
                                    compatibles: [CompatStr::empty(); 4],
                                    compat_count: 0,
                                });
                                break;
                            }
                        }
                    }
                    if let Some(mut d) = descriptor {
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
                        if let Some(irqs) = child.properties().find("interrupts") {
                            d.irq = irqs.as_value::<u32>().ok();
                        }
                        devices[dev_count] = d;
                        dev_count += 1;
                    }
                }
            }

            let info = MachineInfo {
                boot_hart: hart_id,
                cpu_count,
                cpu_info,
                mem_count,
                memory_regions,
                dev_count,
                devices,
            };

            // 只有 boot hart 进入 Core；其余 hart 停在这里（等 Core 未来唤醒）
            if !cpu_info[..cpu_count].iter().any(|c| c.boot_cpu && c.hart_id == hart_id) {
                console::log("bootstrap", "non-boot hart parked\n");
                park_hart();
            }
            console::log("bootstrap", "MachineInfo dump:\n");
            console::print(format_args!("{:#?}\n", info));

            // 本文档镜像范围 → reserved（Core 自己，永久保留）
            let image_start = core::ptr::addr_of!(__bootstrap_start) as usize;
            let image_end = core::ptr::addr_of!(__bootstrap_end) as usize;
            let reserved = [MemoryRegion { base: image_start, size: image_end - image_start }];

            console::log("bootstrap", "BOOT DISCOVERY OK\n");
            console::log("core", "core init: ");
            match kernel::init(&info, &reserved) {
                Ok(()) => {
                    console::puts("OK\n");
                    console::log("core", "BOOT CORE OK\n");
                }
                Err(e) => {
                    console::puts("FAILED: ");
                    console::puts(e);
                    console::puts("\n");
                }
            }
        }
        Err(_) => {
            console::log("bootstrap", "FDT magic: BAD!\n");
        }
    }

    loop {
        core::hint::spin_loop();
    }
}

/// 非 boot hart：WFI 循环（未接内核唤醒前，永久停驻）。
fn park_hart() -> ! {
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    console::puts("BOOTSTRAP PANIC");
    loop {
        core::hint::spin_loop();
    }
}
