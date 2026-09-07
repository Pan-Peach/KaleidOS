use core::arch::global_asm;

use super::console;

global_asm!(include_str!("trap.S"));

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapFrame {
    pub x: [usize; 32],
    pub sstatus: usize,
    pub sepc: usize,
}

pub enum Trap {
    Interrupt(Interrupt),
    Exception(Exception),
}

pub enum Exception {
    InstructionMisaligned,
    InstructionAccessFault,
    IllegalInstruction,
    Breakpoint,
    LoadAddressMisaligned,
    LoadAccessFault,
    StoreAddressMisaligned,
    StoreAccessFault,
    UserEnvCall,
    SupervisorEnvCall,
    InstructionPageFault,
    LoadPageFault,
    StorePageFault,
    Unknown(usize),
}

pub enum Interrupt {
    SupervisorSoft,
    SupervisorTimer,
    SupervisorExternal,
    Unknown(usize),
}

impl Exception {
    pub fn from_code(code: usize) -> Self {
        match code {
            0 => Self::InstructionMisaligned,
            1 => Self::InstructionAccessFault,
            2 => Self::IllegalInstruction,
            3 => Self::Breakpoint,
            4 => Self::LoadAddressMisaligned,
            5 => Self::LoadAccessFault,
            6 => Self::StoreAddressMisaligned,
            7 => Self::StoreAccessFault,
            8 => Self::UserEnvCall,
            9 => Self::SupervisorEnvCall,
            12 => Self::InstructionPageFault,
            13 => Self::LoadPageFault,
            15 => Self::StorePageFault,
            other => Self::Unknown(other),
        }
    }
}

impl Interrupt {
    pub fn from_code(code: usize) -> Self {
        match code {
            1 => Self::SupervisorSoft,
            5 => Self::SupervisorTimer,
            9 => Self::SupervisorExternal,
            other => Self::Unknown(other),
        }
    }
}

struct Scause(usize);

impl Scause {
    fn from_bits(bits: usize) -> Self {
        Scause(bits)
    }

    fn is_interrupt(&self) -> bool {
        (self.0 >> 63) != 0
    }

    fn cause(&self) -> Trap {
        let code = self.0 & !(1usize << 63);

        if self.is_interrupt() {
            Trap::Interrupt(Interrupt::from_code(code))
        } else {
            Trap::Exception(Exception::from_code(code))
        }
    }
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
pub extern "C" fn trap_handler(trap_frame: *mut TrapFrame, raw_scause: usize, stval: usize) -> ! {
    let scause = Scause::from_bits(raw_scause);
    let trap = scause.cause();
    let sepc = unsafe { (*trap_frame).sepc };

    match trap {
        Trap::Interrupt(interrupt) => match interrupt {
            Interrupt::SupervisorSoft | Interrupt::SupervisorTimer | Interrupt::Unknown(_) => {
                console::write_fmt(format_args!(
                    "Unhandled interrupt: scause = {:#x}, sepc = {:#x}, stval = {:#x}\n",
                    raw_scause, sepc, stval
                ));
            }
            Interrupt::SupervisorExternal => {
                // TODO: dispatch through the PLIC/IRQ subsystem.
            }
        },
        Trap::Exception(exception) => match exception {
            _ => {
                console::write_fmt(format_args!(
                    "Unhandled exception: scause = {:#x}, sepc = {:#x}, stval = {:#x}\n",
                    raw_scause, sepc, stval
                ));
            }
        },
    }

    loop {
        core::hint::spin_loop();
    }
}
