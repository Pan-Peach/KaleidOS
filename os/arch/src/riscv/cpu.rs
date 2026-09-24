//! RISC-V CPU implementation.
//!
//! The public type is named for the ISA family.  The XLEN-specific assembly
//! remains in `context/switch64.S`, so adding RV32 does not require renaming
//! the family-level CPU contract again.

use super::{console, firmware, trap};
use crate::{Console, CpuArch, ResetType, SystemReset, Timer};
use core::arch::asm;

pub struct Riscv;

#[cfg(all(feature = "supervisor", not(feature = "machine")))]
const IRQ_ENABLE_BIT: usize = 1 << 1;

#[cfg(all(feature = "machine", not(feature = "supervisor")))]
const IRQ_ENABLE_BIT: usize = 1 << 3;

#[cfg(all(feature = "machine", feature = "supervisor"))]
const IRQ_ENABLE_BIT: usize = 0;

/// RISC-V 寄存器上下文记录（`__switch` 的保存 / 恢复形状）。
///
/// 布局即 ABI：字段顺序 / 偏移必须与 `context/switch64.S`（`sd`/`ld`，8 字节）
/// 和 `context/switch32.S`（`sw`/`lw`，4 字节）**逐字一致**，见下方 `const _`
/// 断言。
///
/// # `tp` 不变式（`docs/architecture/memory-and-heap.md` §5）
///
/// `tp` 承载**当前执行的 per-instance runtime slot**（组件运行时自有的 opaque
/// 状态指针；Core 只存 / 传，从不解释）：
///
/// - `tp == 0` = 无 slot（Core / 尚未安装的实例）；
/// - **Core 是唯一写者**：psABI 把 `tp` 标为 unallocatable/fixed，编译器永不
///   分配或写入它（本镜像无 TLS，也没有任何 TLS 重定位）；
/// - trap 帧**已经**保存 / 恢复 `tp`（`TrapFrame.x[4]`，`trap64.S` / `trap32.S`），
///   本记录负责**上下文切换**路径上的保存 / 恢复；
/// - 这是**执行状态**，不是内存记账：Core 不跟踪指针指向什么，清 slot 不释放
///   任何内存。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiscvContext {
    ra: usize,
    sp: usize,
    s: [usize; 12], // s0-s11
    tp: usize,
}

// 布局即 ABI：把字段偏移钉死在 `__switch` 的 `.S` 常量上（112 / 56 = 14 个字）。
// 断言失败 = 结构体与汇编已经漂移，绝不允许。
const _: () = {
    assert!(core::mem::offset_of!(RiscvContext, ra) == 0);
    assert!(core::mem::offset_of!(RiscvContext, sp) == core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(RiscvContext, s) == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(RiscvContext, tp) == 14 * core::mem::size_of::<usize>());
};

impl CpuArch for Riscv {
    type Context = RiscvContext;
    type IrqFlags = usize;

    fn context_switch(from: &mut Self::Context, to: &Self::Context) {
        unsafe extern "C" {
            fn __switch(from: *mut RiscvContext, to: *const RiscvContext);
        }
        unsafe {
            __switch(from as *mut RiscvContext, to as *const RiscvContext);
        }
    }

    fn new_context(entry: usize, stack_top: usize) -> Self::Context {
        RiscvContext {
            ra: entry,
            sp: stack_top,
            s: [0; 12],
            // 0 = 无 runtime slot：新上下文首启不带任何实例的 runtime context。
            // 边界在切换前用 `set_context_slot` 安装（Core 是唯一写者）。
            tp: 0,
        }
    }

    fn runtime_slot() -> usize {
        let slot: usize;
        // SAFETY: `tp` 被 psABI 标为 unallocatable/fixed —— 编译器从不分配或
        // 写入它，读取只是一条寄存器移动（无内存 / 无栈 / 无标志位）。
        unsafe {
            asm!(
                "mv {}, tp",
                out(reg) slot,
                options(nomem, nostack, preserves_flags),
            );
        }
        slot
    }

    fn install_runtime_slot(slot: usize) {
        // SAFETY: 同上 —— 镜像无 TLS，编译器既不分配也不读取 `tp`，Core 是唯一
        // 写者；写寄存器不影响内存 / 栈 / 标志位。
        unsafe {
            asm!(
                "mv tp, {}",
                in(reg) slot,
                options(nomem, nostack, preserves_flags),
            );
        }
    }

    fn set_context_slot(context: &mut Self::Context, slot: usize) {
        // `__switch` 从目标记录装载 `tp`，所以这是「安装」的**唯一有效**机制：
        // 切入该上下文后，被恢复的执行带着自己的 runtime slot 运行。
        context.tp = slot;
    }

    fn init() {
        trap::init();
    }

    fn disable_irq() -> Self::IrqFlags {
        let mut old: usize = 0;
        unsafe {
            #[cfg(all(feature = "supervisor", not(feature = "machine")))]
            asm!(
                "csrrc {old}, sstatus, {sie}",
                old = out(reg) old,
                sie = const IRQ_ENABLE_BIT,
            );
            #[cfg(all(feature = "machine", not(feature = "supervisor")))]
            asm!(
                "csrrc {old}, mstatus, {mie}",
                old = out(reg) old,
                mie = const IRQ_ENABLE_BIT,
            );
        }
        old
    }

    fn restore_irq(flags: Self::IrqFlags) {
        if flags & IRQ_ENABLE_BIT != 0 {
            unsafe {
                #[cfg(all(feature = "supervisor", not(feature = "machine")))]
                asm!(
                    "csrs sstatus, {mask}",
                    mask = in(reg) IRQ_ENABLE_BIT,
                );
                #[cfg(all(feature = "machine", not(feature = "supervisor")))]
                asm!(
                    "csrs mstatus, {mask}",
                    mask = in(reg) IRQ_ENABLE_BIT,
                );
            }
        }
    }

    fn wait_for_interrupt() {
        unsafe {
            asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

impl Timer for Riscv {
    fn now() -> u64 {
        firmware::time()
    }

    fn set_deadline(_deadline: u64) {
        firmware::set_timer(_deadline);
    }

    fn cancel_deadline() {
        firmware::cancel_timer();
    }

    fn register_timer_handler(handler: extern "C" fn()) {
        trap::register_timer_handler(handler);
    }

    fn enable_timer_interrupt() {
        firmware::enable_timer_interrupt();
    }
}

impl Console for Riscv {
    fn write_byte(byte: u8) {
        console::write_byte(byte);
    }

    fn getc() -> Option<u8> {
        firmware::console_getc()
    }
}

impl SystemReset for Riscv {
    fn system_reset(reset_type: ResetType) -> ! {
        firmware::system_reset(reset_type)
    }
}
