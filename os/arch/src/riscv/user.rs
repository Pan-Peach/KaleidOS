//! RV64 user execution: sret into U, synchronous trap return to the task stack.
//! Core supplies verified frames/AS and owns the active context. No syscall policy.

core::arch::global_asm!(include_str!("user64.S"));

#[derive(Clone, Debug, PartialEq)]
#[repr(C)]
pub struct UserFrame {
    pub x: [usize; 32],
    pub pc: usize,
    pub status: usize,
    pub floating: [u64; 32],
    pub fcsr: usize,
}

impl Default for UserFrame {
    fn default() -> Self {
        Self {
            x: [0; 32],
            pc: 0,
            status: 0x2080,
            floating: [0; 32],
            fcsr: 0,
        }
    }
}

#[repr(C)]
pub struct UserContext {
    caller: [usize; 18], // ra, sp, gp, tp, s0..s11, sstatus, satp
    pub satp: usize,
    pub frame: *mut UserFrame,
    pub task: u32,
    pub cause: usize,
    pub address: usize,
    caller_floating: [u64; 32],
    caller_fcsr: usize,
}

impl UserContext {
    pub fn new(satp: usize, frame: *mut UserFrame, task: u32) -> Self {
        Self {
            caller: [0; 18],
            satp,
            frame,
            task,
            cause: 0,
            address: 0,
            caller_floating: [0; 32],
            caller_fcsr: 0,
        }
    }
}

const _: () = {
    assert!(core::mem::offset_of!(UserFrame, pc) == 256);
    assert!(core::mem::offset_of!(UserFrame, floating) == 272);
    assert!(core::mem::offset_of!(UserFrame, fcsr) == 528);
    assert!(core::mem::offset_of!(UserContext, satp) == 144);
    assert!(core::mem::offset_of!(UserContext, frame) == 152);
    assert!(core::mem::offset_of!(UserContext, cause) == 168);
    assert!(core::mem::offset_of!(UserContext, caller_floating) == 184);
    assert!(core::mem::offset_of!(UserContext, caller_fcsr) == 440);
};

unsafe extern "C" {
    fn user_enter(context: *mut UserContext);
    fn user_return(context: *mut UserContext) -> !;
    fn user_save_floating(frame: *mut UserFrame);
}

/// # Safety
/// Core must validate the U mappings, frame, AS liveness, and task provenance;
/// install its exception hook and keep this context alive until trap return.
pub unsafe fn run(context: &mut UserContext) {
    unsafe { user_enter(context) }
}

/// # Safety
/// Only the real user trap matching this suspended context may call this.
pub unsafe fn stop(
    context: *mut UserContext,
    trap: &super::trap::TrapFrame,
    cause: usize,
    address: usize,
) -> ! {
    let context_ref = unsafe { &mut *context };
    let frame = unsafe { &mut *context_ref.frame };
    frame.x = trap.x;
    frame.x[0] = 0;
    frame.pc = trap.epc;
    frame.status = (trap.status & 0x6000) | 0x80; // U, SPIE; never SUM/SPP
    context_ref.cause = cause;
    context_ref.address = address;
    if trap.status & 0x6000 != 0 {
        unsafe { user_save_floating(context_ref.frame) };
    }
    super::trap::install_scratch_convention();
    unsafe { user_return(context) }
}
