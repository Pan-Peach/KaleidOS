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

use crate::cpu::{CpuId, LocalInterruptHandler};
use core::sync::atomic::{AtomicUsize, Ordering};

/// 当前执行 CPU 的**逻辑**身份。
///
/// 从 CPU-local 入口记录读取已绑定的逻辑 id（UP 恒为 CPU0）。**不**需要改 trap
/// 汇编——分发点在 Rust 侧。
fn current_logical_cpu() -> CpuId {
    use crate::CpuArch;
    // 未绑定的 CPU 是**不变式破坏**，绝不能回退成 `CpuId(0)`——那会把定时器 /
    // 调度 / containment 操作指向错误的 CPU。
    crate::CpuImpl::current_cpu().expect("current CPU is not bound during interrupt dispatch")
}

static TIMER_HANDLER: AtomicUsize = AtomicUsize::new(0);

pub fn register_timer_handler(handler: LocalInterruptHandler) {
    TIMER_HANDLER.store(handler as usize, Ordering::Release);
}

pub fn dispatch_timer() {
    let address = TIMER_HANDLER.load(Ordering::Acquire);
    assert!(address != 0, "timer interrupt handler is not registered");
    // SAFETY: 注册方保证签名与 `LocalInterruptHandler` 一致（单一注册入口）。
    let handler: LocalInterruptHandler = unsafe { core::mem::transmute(address) };
    handler(current_logical_cpu());
}

/// 外部中断回调（Core 在 `irq::init` 时注册 `crate::irq::on_external`）。
static EXTERNAL_HANDLER: AtomicUsize = AtomicUsize::new(0);

pub fn register_external_handler(handler: LocalInterruptHandler) {
    EXTERNAL_HANDLER.store(handler as usize, Ordering::Release);
}

/// 外部中断分发（`SupervisorExternal`/`MachineExternal` trap 分支调用）。
/// 具体 claim/dispatch/complete 由注册进来的 Core handler 完成。
pub fn dispatch_external() {
    let address = EXTERNAL_HANDLER.load(Ordering::Acquire);
    assert!(address != 0, "external interrupt handler is not registered");
    // SAFETY: 注册方保证签名与 `LocalInterruptHandler` 一致（单一注册入口）。
    let handler: LocalInterruptHandler = unsafe { core::mem::transmute(address) };
    handler(current_logical_cpu());
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrapFrame {
    pub x: [usize; 32],
    pub status: usize,
    pub epc: usize,
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
    // M-mode 中断码（`mcause` 与 `scause` 编码一致）：共享解码器不隐含
    // S-mode——M-mode profile 复用同一套 `from_code`。
    MachineSoft,
    MachineTimer,
    MachineExternal,
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
            3 => Self::MachineSoft,
            7 => Self::MachineTimer,
            11 => Self::MachineExternal,
            other => Self::Unknown(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Interrupt, Trap};

    #[test]
    fn shared_decode_handles_both_privilege_modes() {
        // S-mode 中断（现有行为不变）
        assert!(matches!(
            Trap::Interrupt(Interrupt::from_code(5)),
            Trap::Interrupt(Interrupt::SupervisorTimer)
        ));
        // M-mode 中断（共享解码，不隐含 S-mode）
        assert!(matches!(
            Trap::Interrupt(Interrupt::from_code(7)),
            Trap::Interrupt(Interrupt::MachineTimer)
        ));
        assert!(matches!(
            Trap::Interrupt(Interrupt::from_code(11)),
            Trap::Interrupt(Interrupt::MachineExternal)
        ));
    }
}

/// 普通 Core trap 路径的异常钩子（Core 注册；见 `supervisor::trap_handler`）。
///
/// `true` = 钩子已处理（汇编按（可能被修改过的）`TrapFrame` 恢复并 `sret`）；
/// `false` / 未注册 = fatal（保持 Core 不变式：未证明可恢复的异常一律 panic）。
pub type ExceptionHook = fn(frame: *mut TrapFrame, cause: usize, stval: usize) -> bool;

static EXCEPTION_HOOK: AtomicUsize = AtomicUsize::new(0);

/// 注册异常钩子（后注册覆盖先注册；Core 只注册一次）。
pub fn register_exception_hook(hook: ExceptionHook) {
    EXCEPTION_HOOK.store(hook as usize, Ordering::Release);
}

/// 把一次异常交给已注册的钩子；未注册 → `false`（fatal）。
pub(crate) fn dispatch_exception(frame: *mut TrapFrame, cause: usize, stval: usize) -> bool {
    let address = EXCEPTION_HOOK.load(Ordering::Acquire);
    if address == 0 {
        return false;
    }
    // SAFETY: 注册方保证签名与 `ExceptionHook` 一致（单一注册入口）。
    let hook: ExceptionHook = unsafe { core::mem::transmute(address) };
    hook(frame, cause, stval)
}

/// 安全 trap 栈：**所有** S-mode trap 先切到这里（见 `trap32.S` / `trap64.S`
/// 的 sscratch 约定），再用调用者的栈做处理；组件栈 / 任务栈因此永远不会被
/// trap 帧写坏。
///
/// rv64 的 trap 栈是 **per-CPU** 的（在 `cpu::PerCpu` 里，`entry` 紧贴栈顶），
/// 由 boot 装进 `sscratch`；下面的 `TRAP_STACK` 仅供 rv32（无 SMP，单张栈）。
pub const TRAP_STACK_BYTES: usize = 32 * 1024;

#[cfg(target_arch = "riscv32")]
const STACK_ALIGNMENT: usize = 16;

#[cfg(target_arch = "riscv32")]
#[repr(align(16))]
struct TrapStack([u8; TRAP_STACK_BYTES]);

#[cfg(target_arch = "riscv32")]
static mut TRAP_STACK: TrapStack = TrapStack([0; TRAP_STACK_BYTES]);

/// 安全 trap 栈的半开区间 `[base, top)`（诊断 / ArchTest 断言）。
///
/// rv64：CPU0 的 per-CPU trap 栈（ArchTest / isolated 都跑在 CPU0 上）。
pub fn trap_stack_range() -> (usize, usize) {
    #[cfg(target_arch = "riscv64")]
    {
        super::cpu::trap_stack_range_for(0)
    }
    #[cfg(target_arch = "riscv32")]
    {
        // SAFETY: 只取静态数组地址（不读内容、不创建引用）。
        let base = unsafe { core::ptr::addr_of!(TRAP_STACK.0) } as usize;
        (base, base + TRAP_STACK_BYTES)
    }
}

/// 安全 trap 栈顶（16 字节对齐）：装在 `sscratch` 里，trap 入口据此换栈。
pub fn trap_stack_top() -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        trap_stack_range().1
    }
    #[cfg(target_arch = "riscv32")]
    {
        trap_stack_range().1 & !(STACK_ALIGNMENT - 1)
    }
}

/// 重新装入 trap 栈约定（`sscratch` = 安全栈顶）。
///
/// 放弃路径（`trampoline_return` 不 `sret`）必须显式恢复该约定：外层 trap
/// 已被放弃，不会再由 trap 出口恢复它。
#[cfg(all(feature = "supervisor", not(feature = "machine")))]
pub fn install_scratch_convention() {
    #[cfg(target_arch = "riscv64")]
    {
        // 统一约定下 `sscratch` 恒 = `&entry`（trap 入口装入、处理期间不变），
        // 放弃路径无需恢复它。
    }
    #[cfg(target_arch = "riscv32")]
    {
        let top = trap_stack_top();
        // SAFETY: 只写 CSR；无内存 / 栈副作用。
        unsafe {
            core::arch::asm!("csrw sscratch, {top}",
                top = in(reg) top,
                options(nostack, preserves_flags),
            );
        }
    }
}

/// cause 寄存器解码（`Scause` 名称沿用 S-mode；M-mode 下语义相同，
/// 只是中断位位置与寄存器名不同，由 `machine` 模块自行读取）。
pub(crate) struct Scause(usize);

impl Scause {
    pub(crate) fn from_bits(bits: usize) -> Self {
        Scause(bits)
    }

    fn is_interrupt(&self) -> bool {
        (self.0 >> (usize::BITS - 1)) != 0
    }

    pub(crate) fn cause(&self) -> Trap {
        let code = self.0 & !(1usize << (usize::BITS - 1));

        if self.is_interrupt() {
            Trap::Interrupt(Interrupt::from_code(code))
        } else {
            Trap::Exception(Exception::from_code(code))
        }
    }
}

#[cfg(all(feature = "machine", not(feature = "supervisor")))]
pub mod machine;
#[cfg(all(feature = "supervisor", not(feature = "machine")))]
pub mod supervisor;

#[cfg(all(feature = "machine", not(feature = "supervisor")))]
pub use machine::init;
#[cfg(all(feature = "supervisor", not(feature = "machine")))]
pub use supervisor::init;
#[cfg(all(feature = "supervisor", not(feature = "machine")))]
pub use supervisor::vector_address;

#[cfg(all(feature = "machine", feature = "supervisor"))]
pub fn init() {}
