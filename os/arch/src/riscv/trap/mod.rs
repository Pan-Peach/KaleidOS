//! RISC-V trap 入口与解码。
//!
//! 按 privilege mode 拆成两个实现模块（编译期选实现，不需要动态抽象——
//! 与 `entry32.S/entry64.S` 同一思路）：
//!
//! - `supervisor`：S-mode（`stvec`/`scause`/`sepc`/`stval`），当前唯一实现；
//! - `machine`：M-mode（`mtvec`/`mcause`/`mepc`/`mtval`），骨架待实现。
//!
//! 两模式共享的**解码**部分留在本文件：`TrapFrame`、`Trap`/`Exception`/
//! `Interrupt` 与 `Scause`（cause 编码在 S/M 模式一致，只差寄存器名与
//! 中断位位置的处理方式）。

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

/// cause 寄存器解码（`Scause` 名称沿用 S-mode；M-mode 下语义相同，
/// 只是中断位位置与寄存器名不同，由 `machine` 模块自行读取）。
struct Scause(usize);

impl Scause {
    fn from_bits(bits: usize) -> Self {
        Scause(bits)
    }

    fn is_interrupt(&self) -> bool {
        (self.0 >> (usize::BITS - 1)) != 0
    }

    fn cause(&self) -> Trap {
        let code = self.0 & !(1usize << (usize::BITS - 1));

        if self.is_interrupt() {
            Trap::Interrupt(Interrupt::from_code(code))
        } else {
            Trap::Exception(Exception::from_code(code))
        }
    }
}

pub mod machine;
pub mod supervisor;

/// S-mode trap 安装入口（当前唯一实现；`cpu.rs` 的 `CpuImpl::init` 调用）。
pub use supervisor::init;
