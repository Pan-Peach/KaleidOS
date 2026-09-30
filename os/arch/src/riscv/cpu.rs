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

/// 编译期 per-CPU 容量：唯一定义在 crate 根（`arch/build.rs` 从 Kconfig
/// `MAX_CPUS` 生成），这里 re-export 供本模块与 `plic` 复用。
pub use crate::MAX_CPUS;

/// RISC-V 寄存器上下文记录（`__switch` 的保存 / 恢复形状）。
///
/// 布局即 ABI：字段顺序 / 偏移必须与 `context/switch64.S`（`sd`/`ld`，8 字节）
/// 和 `context/switch32.S`（`sw`/`lw`，4 字节）**逐字一致**，见下方 `const _`
/// 断言。
///
/// # `tp` 不变式
///
/// `tp` 是 RISC-V 的 thread pointer：**普通架构执行状态**，不是组件身份或
/// per-instance 上下文。本镜像没有 TLS，也没有任何 TLS 重定位，因此 psABI 把
/// `tp` 标为 unallocatable/fixed——编译器永不分配或写入它；唯一的写入者是显式
/// 汇编（本记录在切换路径上的保存 / 恢复、同步跨 AS 进入时的显式清零）。
///
/// - `tp == 0` 是全新执行上下文的起点（`new_context`）；
/// - trap 帧**已经**保存 / 恢复 `tp`（`TrapFrame.x[4]`，`trap64.S` / `trap32.S`），
///   本记录负责**上下文切换**路径上的保存 / 恢复；
/// - Core 从不解释它，也不据此做任何身份 / 授权判断。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiscvContext {
    ra: usize,
    sp: usize,
    s: [usize; 12], // s0-s11
    tp: usize,
}

#[repr(C)]
pub struct CpuEntry {
    pub trap_stack_top: usize,
    pub core_base: usize,
    pub logical_id: usize,
    pub hardware_id: usize,
}

impl CpuEntry {
    pub const fn empty() -> Self {
        Self {
            trap_stack_top: 0,
            core_base: 0,
            logical_id: 0,
            hardware_id: 0,
        }
    }
}

/// 每 CPU 的入口记录 + 它自己的安全 trap 栈。
///
/// `entry` 紧贴在 `stack` 之上，所以 **`&entry` 就等于这张栈的栈顶**。
/// 于是 `sscratch`（= `&entry`）**一个值**同时给出「入口记录指针」和
/// 「trap 栈顶」——trap 入口不需要任何临时寄存器就能换栈并解析身份。
///
/// 布局即 ABI：trap 入口假定 `sscratch` 即栈顶（`T = &entry`）。
#[cfg(target_arch = "riscv64")]
#[repr(C, align(16))]
struct PerCpu {
    stack: [u8; super::trap::TRAP_STACK_BYTES],
    entry: CpuEntry,
}

#[cfg(target_arch = "riscv64")]
static mut PER_CPU: [PerCpu; MAX_CPUS] = [const {
    PerCpu {
        stack: [0; super::trap::TRAP_STACK_BYTES],
        entry: CpuEntry::empty(),
    }
}; MAX_CPUS];

/// 逻辑 CPU `i` 的入口记录地址（== 它的 trap 栈顶）。
#[cfg(target_arch = "riscv64")]
fn entry_ptr(i: usize) -> *mut CpuEntry {
    // SAFETY: 只取静态数组元素地址（不创建引用）。
    unsafe { core::ptr::addr_of_mut!(PER_CPU[i].entry) }
}

/// 逻辑 CPU `i` 的安全 trap 栈半开区间 `[base, top)`。
///
/// `entry` 紧贴 `stack` 之上，所以 `top == &entry`（trap 入口据此换栈）。
#[cfg(target_arch = "riscv64")]
pub fn trap_stack_range_for(i: usize) -> (usize, usize) {
    // SAFETY: 只取静态数组元素地址。
    let base = unsafe { core::ptr::addr_of!(PER_CPU[i].stack) } as usize;
    let top = unsafe { core::ptr::addr_of!(PER_CPU[i].entry) } as usize;
    (base, top)
}

// 布局即 ABI：把字段偏移钉死在 `__switch` 的 `.S` 常量上（112 / 56 = 14 个字）。
// 断言失败 = 结构体与汇编已经漂移，绝不允许。
const _: () = {
    assert!(core::mem::offset_of!(RiscvContext, ra) == 0);
    assert!(core::mem::offset_of!(RiscvContext, sp) == core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(RiscvContext, s) == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(RiscvContext, tp) == 14 * core::mem::size_of::<usize>());
};

#[cfg(target_arch = "riscv64")]
#[inline]
fn read_scratch() -> usize {
    let value: usize;
    unsafe {
        asm!(
            "csrr {value}, sscratch",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

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
            // 全新执行上下文：tp 从 0 起步（本镜像无 TLS；tp 只是被透明保存 /
            // 恢复的架构执行状态，不承载任何实例上下文）。
            tp: 0,
        }
    }

    fn init_cpu() {
        trap::init();
    }

    fn enable_irq() {
        // 打开当前 CPU 的全局中断使能位（S-mode `sstatus.SIE` / M-mode `mstatus.MIE`）。
        // 各本地中断源（timer/external/IPI）应先各自 unmask，最后再调这里。
        unsafe {
            #[cfg(all(feature = "supervisor", not(feature = "machine")))]
            asm!("csrs sstatus, {mask}", mask = in(reg) IRQ_ENABLE_BIT);
            #[cfg(all(feature = "machine", not(feature = "supervisor")))]
            asm!("csrs mstatus, {mask}", mask = in(reg) IRQ_ENABLE_BIT);
        }
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
        // 原始 idle 提示：不 arm timer、不改中断使能状态，可能立刻返回或
        // 永不返回（调用者契约见 `CpuArch::wait_for_interrupt`）。
        unsafe {
            asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }

    unsafe fn atomic_idle(flags: Self::IrqFlags) {
        // 进入时本 CPU 中断投递已关闭（trait 契约），唤醒源已在关中断状态下
        // armed。这里**保持**全局中断关闭执行 WFI：RISC-V WFI 对"本地使能
        // （如 `sie.STIE`）且 pending"的中断即使在 SIE=0 时也必须返回，因此
        // 唤醒不会在"使能 → 睡眠"窗口里丢失——pending 的 trap 留到
        // `restore_irq` 之后按正常路径处理。
        if flags & IRQ_ENABLE_BIT != 0 {
            unsafe {
                asm!("wfi", options(nomem, nostack, preserves_flags));
            }
        }
        // `flags` 为关中断时不做任何事：没有可用唤醒源，绝不睡眠。
        Self::restore_irq(flags);
    }

    // ——— CPU-local 身份与基址 ———
    //
    // 统一约定（rv64）：`sscratch` 恒 = 本 CPU 的 `CpuEntry*`，而 `PerCpu` 把
    // `entry` 紧贴在 trap 栈之上，所以这个指针**同时**是「入口记录指针」和
    // 「安全 trap 栈顶」。trap 入口、`current_cpu`、`per_cpu_base` 都只读它。
    // UP 是「只有 CPU0 的 SMP」：boot 无条件给 CPU0 装一次记录即可，不再有
    // `smp` feature 的语义分叉。RV32 目前无 SMP，保持旧约定（见下）。
    //
    // `tp`（任务执行状态）与 per-CPU 基址严格分离：本方法不动 `tp`。

    fn current_cpu() -> Option<crate::cpu::CpuId> {
        #[cfg(target_arch = "riscv64")]
        {
            // `sscratch` 装的是入口记录**指针**（不是下标）：0 = 未绑定。
            let p = read_scratch();
            if p == 0 {
                None
            } else {
                // SAFETY: 非 0 即指向本 CPU 的 `CpuEntry`（只由 install 写入）。
                let entry = unsafe { &*(p as *const CpuEntry) };
                Some(crate::cpu::CpuId::from_raw(entry.logical_id))
            }
        }
        #[cfg(target_arch = "riscv32")]
        {
            // RV32 无 SMP：恒为唯一逻辑 CPU，不读 `sscratch`（仍是旧约定）。
            Some(crate::cpu::CpuId::from_raw(0))
        }
    }

    fn per_cpu_base() -> Option<core::ptr::NonNull<()>> {
        #[cfg(target_arch = "riscv64")]
        {
            let p = read_scratch();
            if p == 0 {
                None
            } else {
                // SAFETY: 同上；core_base 由 Core 提供的非空指针写入。
                let entry = unsafe { &*(p as *const CpuEntry) };
                core::ptr::NonNull::new(entry.core_base as *mut ())
            }
        }
        #[cfg(target_arch = "riscv32")]
        {
            None
        }
    }

    unsafe fn install_per_cpu_base(cpu: crate::cpu::CpuId, base: core::ptr::NonNull<()>) {
        #[cfg(target_arch = "riscv64")]
        {
            let i = cpu.raw();
            assert!(
                i < MAX_CPUS,
                "logical CpuId {} exceeds arch MAX_CPUS {}",
                i,
                MAX_CPUS
            );

            // SAFETY: 只在本 CPU、关中断、online 之前写自己的槽（trait 契约）。
            let entry = entry_ptr(i);
            unsafe {
                (*entry).core_base = base.as_ptr() as usize;
                (*entry).logical_id = i;
                // `&entry` 就是本 CPU 的 trap 栈顶（见 `PerCpu`）。
                (*entry).trap_stack_top = entry as usize;
                // hardware_id 暂不填（trait 未携带硬件身份；读者也不需要）。

                // 把入口记录指针装进 `sscratch`：trap 入口据此换栈 + 解析身份。
                asm!("csrw sscratch, {}", in(reg) entry as usize, options(nostack, preserves_flags));
            }
        }
        #[cfg(target_arch = "riscv32")]
        {
            let _ = (cpu, base);
        }
    }
}

impl Timer for Riscv {
    fn init_cpu() -> Result<(), crate::TimerError> {
        // UP：无 per-CPU timer 状态；deadline 编程见 `firmware::set_timer`。
        // SMP：实现时在此初始化本 CPU 的 `mtimecmp` 路径。
        Ok(())
    }

    fn now() -> u64 {
        firmware::time()
    }

    fn set_deadline(deadline: u64) -> Result<(), crate::TimerError> {
        firmware::set_timer(deadline)
    }

    fn cancel_deadline() {
        firmware::cancel_timer();
    }

    fn register_timer_handler(handler: crate::cpu::LocalInterruptHandler) {
        trap::register_timer_handler(handler);
    }

    fn enable_timer_interrupt() -> Result<(), crate::TimerError> {
        firmware::enable_timer_interrupt();
        Ok(())
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
