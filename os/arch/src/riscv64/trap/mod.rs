use core::arch::global_asm;

global_asm!(include_str!("trap.S"));

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapFrame {
    pub x: [usize; 32],
    pub sstatus: usize,
    pub sepc: usize,
}

pub fn init() {
    unsafe {
        set_trap_vector();
    }
}

unsafe fn set_trap_vector() {
    unsafe extern "C" {
        static trap_vec: u8;
    }

    let addr = core::ptr::addr_of!(trap_vec) as usize;

    unsafe {
        core::arch::asm!("csrw stvec, {addr}",
            addr = in(reg) addr,
            options(nostack, preserves_flags),
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn trap_handler(trap_frame: *mut TrapFrame, scause: usize, stval: usize) {
    loop {
        core::hint::spin_loop();
    }
}
