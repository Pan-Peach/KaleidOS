#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;
use kernel::machine::{CpuInfo, MachineInfo, MemoryRegion};

mod console;
mod sbi;

global_asm!(include_str!("entry.S"));

#[unsafe(no_mangle)]
extern "C" fn bootstrap_main(hart_id: usize, dtb_pa: usize) -> ! {
    console::log("bootstrap", "KaleidOS bootstrap\n");
    console::log("bootstrap", "========================================\n");

    // FDT 发现：直接吃 OpenSBI 给的 dtb 物理地址（unsafe：该地址有效性 Rust 无从验证）
    match unsafe { fdt::Fdt::from_ptr_unaligned(dtb_pa as *const u8) } {
        Ok(tree) => {
            console::log("bootstrap", "FDT magic: OK\n");

            // 归一化：fdt 类型 → core::machine 类型（单镜像内函数调用，无需 POD 协议）
            let mut memory_regions = [MemoryRegion { base: 0, size: 0 }; 16];
            let mut cpu_info = [CpuInfo { boot_cpu: false, hart_id: 0 }; 8];

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

            let info = MachineInfo {
                boot_hart: hart_id,
                cpu_info: &cpu_info[..cpu_count],
                memory_regions: &memory_regions[..mem_count],
                devices: &[],
            };

            console::log("bootstrap", "MachineInfo dump:\n");
            console::print(format_args!("{:#?}\n", info));

            console::log("bootstrap", "BOOT DISCOVERY OK\n");
            console::log("core", "core init: ");
            match kernel::init(&info) {
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

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    console::puts("BOOTSTRAP PANIC");
    loop {
        core::hint::spin_loop();
    }
}
