//! Architecture backend crate: RISC-V ISA family plus host fake backend.
//!
//! 组织方式参照 Linux `arch/<isa>/` 与 seL4 `src/arch/{arm,riscv,x86}`：
//! 每个 ISA 一个独立单元，内部只放指令集相关代码：
//! trap 入口与分发、寄存器上下文与 context switch、MMU/TLB、
//! 用户态模式切换、中断开关、原子/CPU 原语。
//!
//! M0 目标：启动入口（`_start`，接收 a0=hartid、a1=dtb 指针）与最小实现。
//! 后续架构：x86_64 / aarch64 / loongarch64（各自独立 crate）。

#![no_std]
extern crate alloc;

#[cfg(all(feature = "supervisor", feature = "machine"))]
compile_error!("arch features `supervisor` and `machine` are mutually exclusive");

#[cfg(not(any(feature = "supervisor", feature = "machine")))]
compile_error!("arch requires exactly one privilege-mode feature: `supervisor` or `machine`");

#[cfg(test)]
extern crate std;

pub mod component;
pub mod nommu;
pub mod store;
pub mod vm;

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
pub mod fake;

// riscv 模块在所有目标都编译，但内部子模块按依赖门控：
// - ISA/asm 子模块（cpu/context/trap/firmware/console）只在真实 RISC-V 目标；
// - 纯算法子模块（elf 重定位、mmu 页表编码）host 也可编译 —— host 测试直接测
//   生产实现，而不是一份复制算法（见 docs/testing.md）。
// boot 期的内核页表策略（identity + high-half 双映射、段权限、临时 root）在
// boot crate `vm/`，不属于 arch。
pub mod riscv;

pub use store::{ComponentStore, StoreEntry, StoreError};

/// Component object ABI 由 RISC-V 实现提供（纯字节/编码逻辑，任何目标都可编译，
/// host 下行为与 riscv 目标一致——`normalize_symbol_address` 在 host 是恒等映射）。
pub type ComponentRelocationImpl = riscv::elf::RiscvRelocator;

/// Normalize a linked high-half address to its early identity/physical view.
///
/// Early boot keeps the whole RAM window identity-mapped, so the low alias of
/// a high-half kernel symbol is reachable from a low-address component (an
/// auipc+jalr pair only covers ±2 GiB).  Host builds have no high-half split
/// and use the address as-is.
///
/// 这是 arch 的地址方案原语（ELf relocator `normalize_symbol_address` 与 boot
/// selftest 都要用）；boot 侧的映射策略在 boot crate `vm/`，那里的
/// `HIGH_HALF_OFFSET` 与本处**必须保持一致**（由 linker.ld 布局决定）。
#[cfg(target_arch = "riscv64")]
const HIGH_HALF_OFFSET: usize = 0xffff_ffc0_0000_0000;

#[cfg(target_arch = "riscv64")]
pub fn physical_address_of(address: usize) -> usize {
    if address >= HIGH_HALF_OFFSET {
        address.wrapping_sub(HIGH_HALF_OFFSET)
    } else {
        address
    }
}

#[cfg(target_arch = "riscv32")]
pub fn physical_address_of(address: usize) -> usize {
    address
}

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
pub fn physical_address_of(address: usize) -> usize {
    address
}

pub enum ResetType {
    Shutdown,
    ColdReboot,
    WarmReboot,
}

/// CPU/ISA 原语：上下文、寄存器切换、中断开关和架构初始化。
///
/// 这个 trait 不包含 console、reset 或设备操作；那些属于 firmware/platform
/// 服务，由同一个具体 backend 分别实现相应的 trait。
pub trait CpuArch {
    /// 寄存器上下文类型。
    type Context;
    /// 中断开关的保存状态（RISC-V：`sstatus.SIE`；host fake：`()`）。
    /// C5 骨架：irq-save 临界区的状态载体。
    type IrqFlags;
    fn context_switch(from: &mut Self::Context, to: &Self::Context);
    fn new_context(entry: usize, stack_top: usize) -> Self::Context;
    fn init();
    /// 关中断并返回先前状态（irq-save 临界区进入）。
    /// TODO(C5): Riscv 实现 = `sstatus.SIE` 保存 + 清零；fake = no-op。
    fn disable_irq() -> Self::IrqFlags;
    /// 恢复 `disable_irq` 返回的状态（irq-restore 退出）。
    fn restore_irq(flags: Self::IrqFlags);
}

/// 时钟 / 单次定时器服务（firmware/platform 能力，不是 ISA 原语）。
///
/// 与 `Console`/`SystemReset` 同一模式：Core 只依赖本 trait 与 `TimerImpl`，
/// 不感知 SBI/CLINT 细节。时间单位 = 平台的 timebase tick。
/// C5 骨架：签名即契约，实现待手写。
pub trait Timer {
    /// 当前时间（单调递增）。
    fn now() -> u64;
    /// 编程下一次时钟中断的**绝对** deadline（与 `now` 同一基准）。
    fn set_deadline(deadline: u64);
    /// 取消当前 deadline，直到下一次 `set_deadline` 不再产生 timer IRQ。
    fn cancel_deadline();
    /// 注册时钟中断回调，并打开当前特权级的 timer interrupt。
    fn register_timer_handler(handler: extern "C" fn());
    fn enable_timer_interrupt();
}

/// 早期 console 服务。它是 boot/firmware 传输能力，不是 CPU ISA 原语。
pub trait Console {
    fn write_byte(byte: u8);
    fn getc() -> Option<u8>;
}

/// 系统 reset 服务。具体实现通常来自 SBI、UEFI 或平台固件。
pub trait SystemReset {
    fn system_reset(reset_type: ResetType) -> !;
}

/// 当前编译目标的 CPU backend（host → Fake，RISC-V → Riscv）。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type CpuImpl = riscv::Riscv;

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
pub type CpuImpl = fake::Fake;

/// 当前编译目标的 console backend。
///
/// 现在与 CPU backend 共享具体类型；当同一 ISA 支持多个 platform 时，
/// 这里可以改成由 boot profile 选择，而不改变 Core 的 Console trait。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type ConsoleImpl = riscv::Riscv;

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
pub type ConsoleImpl = fake::Fake;

/// 当前编译目标的 reset backend。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type ResetImpl = riscv::Riscv;

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
pub type ResetImpl = fake::Fake;

/// 当前编译目标的时钟/定时器 backend（C5 骨架；与 Console/Reset 同模式）。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type TimerImpl = riscv::Riscv;

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
pub type TimerImpl = fake::Fake;

/// Core 使用的任务上下文类型；Core 不关心具体 ISA 的寄存器布局。
pub type ContextImpl = <CpuImpl as CpuArch>::Context;
