#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;

mod console;
mod sbi;

global_asm!(include_str!("entry.S"));

#[unsafe(no_mangle)]
extern "C" fn bootstrap_main(hart_id: usize, dtb_pa: usize) -> ! {
    console::puts("Hello, RISC-V!\n");
    console::puts("bootstrapping...\n");

    let _ = hart_id;
    let _ = dtb_pa;

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
