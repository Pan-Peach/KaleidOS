#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

mod console;
mod sbi;

global_asm!(include_str!("entry.S"));

#[unsafe(no_mangle)]
extern "C" fn bootstrap_main(hart_id: usize, dtb_pa: usize) -> ! {
    console::puts("KaleidOS bootstrap\n");
    console::puts("========================================\n");
    console::puts("hart = ");
    console::put_hex(hart_id as u64);
    console::puts("\ndtb  = ");
    console::put_hex(dtb_pa as u64);
    console::puts("\n\n");

    // FDT 发现：直接吃 OpenSBI 给的 dtb 物理地址（unsafe：该地址有效性 Rust 无从验证）
    match unsafe { fdt::Fdt::from_ptr_unaligned(dtb_pa as *const u8) } {
        Ok(tree) => {
            console::puts("FDT magic: OK\n");
            console::puts("FDT size : ");
            console::put_hex(tree.total_size() as u64);
            console::puts("\n");

            // 从 /memory 读 RAM 基址+大小（QEMU virt: base=0x80000000 size=128M）
            if let Some(Ok(region)) = tree.root().memory().reg().iter::<u64, u64>().next() {
                console::puts("RAM base = ");
                console::put_hex(region.address);
                console::puts("\nRAM size = ");
                console::put_hex(region.len);
                console::puts("\n");
            }

            console::puts("\nBOOT DISCOVERY OK\n");
        }
        Err(_) => {
            console::puts("\nFDT magic: BAD!\n");
        }
    }

    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    console::puts("BOOTSTRAP PANIC");
    loop {
        core::hint::spin_loop();
    }
}
