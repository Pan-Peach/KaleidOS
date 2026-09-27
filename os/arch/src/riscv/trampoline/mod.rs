//! 跨地址空间的**最小执行原语**：同步进入目标私有 AS、运行一个入口、回到
//! 挂起的 Core 调用者（正常返回或被 Core trap 路径放弃）。
//!
//! 它**只**做机制：保存 / 恢复调用者的同步 ABI 现场（`ra/sp/gp/tp/s0-s11` +
//! `sstatus` + `satp`）、按需切 `satp` + 全量 `sfence.vma`、装目标栈 / `tp`、
//! 交付入口参数 `a0..a3`、收集返回值。它**不**知道 endpoint / service /
//! lifecycle / registry，也没有 trap 帧、相位机、故障策略或 `stvec`
//! 切换：Core 代码 / 栈 / 全局状态在每个 Isolated AS 里 same VA → same PA
//! （`memory/kernel_mappings.rs`），因此 `stvec` 保持 `trap::vector_address()`
//! 不变、trap 走**普通** Core trap 路径。
//!
//! ```text
//! Core 调用者（任意 AS）                       目标实例 AS
//! isolated::enter(ctx)
//!    │  trampoline_enter(ctx)                    ← 保存调用者现场
//!    │  satp = ctx.instance_satp（不同才切 + sfence）
//!    │  sp = ctx.stack_top, tp = ctx.runtime_slot, sstatus.SIE = ctx.interrupts
//!    └─ jalr ctx.entry ─────────────────────────► 组件运行（satp = 实例 root，
//!                                                  stvec = 普通 Core 向量）
//!    ┌─ 组件 ret ────────────────────────────────┘  result = Returned(a0)
//!    ├─ Core trap 路径判定 Abandon ──► trampoline_return(ctx)   result = Faulted
//!    ▼
//! 恢复 ctx.caller_satp + sfence，恢复调用者 ABI 现场，ret 回 isolated::enter
//! ```
//!
//! **上下文记录是每次调用的**（由 `isolated::enter` 放在自己的栈帧里），不是
//! 单例：嵌套调用（未来的 A→B）各自持有独立的 ctx，恢复顺序天然 LIFO。
//!
//! # 诚实边界
//!
//! 协作式、非对抗边界：S-mode 组件与 Core 同特权级，可以直接改 `satp` /
//! `stvec` / 自己的映射。ASID 恒 0 + 全量 `sfence.vma`。

use super::mmu::SatpActivation;
use super::trap;

#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(include_str!("trampoline64.S"));

#[cfg(target_arch = "riscv32")]
core::arch::global_asm!(include_str!("trampoline32.S"));

/// 一次同步切换的输入（Core 侧组装；全部是 `Copy` 值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    /// 目标实例 AS 的预打包 satp。
    pub activation: SatpActivation,
    /// 目标入口 VA（目标 AS 内可执行，Core 已验证）。
    pub entry: usize,
    /// 目标栈顶 VA（目标 AS 内可读写，16 字节对齐）。
    pub stack_top: usize,
    /// 目标运行时的 runtime slot（写入 `tp`；0 = 无 slot）。
    pub runtime_slot: usize,
    /// 目标初始是否开中断（`sstatus.SIE`）。
    pub interrupts_enabled: bool,
    /// 入口参数（Core 解释其含义，本模块只搬运）。
    pub arg0: usize,
    pub arg1: usize,
    pub arg2: usize,
    pub arg3: usize,
}

/// 同步切换的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 组件正常返回；`usize` = 组件返回值（`a0`）。
    Returned(usize),
    /// Core trap 路径判定不可恢复：控制权已由 [`return_to_core`] 交回进入前的
    /// Core 调用者。
    Faulted,
}

/// 一次调用的执行上下文（**每次调用独立**；布局即 ABI，与 `.S` 的 `.equ`
/// 偏移逐字一致，见下方 `const _` 断言）。
#[repr(C)]
pub struct Context {
    caller_ra: usize,
    caller_sp: usize,
    caller_gp: usize,
    caller_tp: usize,
    caller_s: [usize; 12],
    caller_sstatus: usize,
    caller_satp: usize,
    instance_satp: usize,
    entry: usize,
    stack_top: usize,
    runtime_slot: usize,
    interrupts: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    /// 0 = 正常返回（`a0_result` 有效），1 = Core 放弃。
    result: usize,
    a0_result: usize,
}

// 偏移即 ABI：与 `trampoline64.S` / `trampoline32.S` 顶部 `.equ` 常量一致。
const _: () = {
    assert!(core::mem::offset_of!(Context, caller_ra) == 0);
    assert!(core::mem::offset_of!(Context, caller_sp) == core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, caller_gp) == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, caller_tp) == 3 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, caller_s) == 4 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, caller_sstatus) == 16 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, caller_satp) == 17 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, instance_satp) == 18 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, entry) == 19 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, stack_top) == 20 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, runtime_slot) == 21 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, interrupts) == 22 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, arg0) == 23 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, arg1) == 24 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, arg2) == 25 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, arg3) == 26 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, result) == 27 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(Context, a0_result) == 28 * core::mem::size_of::<usize>());
};

impl Context {
    /// 组装一次调用的上下文（调用者现场由汇编进入时填写）。
    pub const fn new(transition: Transition) -> Self {
        Self {
            caller_ra: 0,
            caller_sp: 0,
            caller_gp: 0,
            caller_tp: 0,
            caller_s: [0; 12],
            caller_sstatus: 0,
            caller_satp: 0,
            instance_satp: transition.activation.satp(),
            entry: transition.entry,
            stack_top: transition.stack_top,
            runtime_slot: transition.runtime_slot,
            interrupts: if transition.interrupts_enabled { 1 } else { 0 },
            arg0: transition.arg0,
            arg1: transition.arg1,
            arg2: transition.arg2,
            arg3: transition.arg3,
            result: 0,
            a0_result: 0,
        }
    }

    /// 目标 AS 的预打包 satp（trap 路径归因比较用）。
    pub fn instance_satp(&self) -> usize {
        self.instance_satp
    }
}

unsafe extern "C" {
    /// 同步进入 `ctx` 的目标 AS；`ret` 时返回（正常或由 `trampoline_return`
    /// 交回）。结果写入 `ctx.result` / `ctx.a0_result`。
    fn trampoline_enter(ctx: *mut Context) -> usize;
    /// 放弃被打断的执行，恢复 `ctx` 的调用者现场并 `ret` 回 `isolated::enter`
    /// 的调用点（永不返回本调用者）。
    fn trampoline_return(ctx: *mut Context) -> !;
}

/// 进入目标私有 AS 执行入口，直到组件返回或 Core 判定放弃。
///
/// `ctx` 位于调用者栈帧；进入后组件在自己的栈上运行，本函数（以及它所在的
/// Core 栈帧）在切换期间挂起，返回 / 放弃时恢复。
pub fn enter(ctx: &mut Context) -> Outcome {
    // SAFETY: ctx 是调用者栈帧里的有效记录；进入前 Core 已校验目标入口 / 栈 /
    // 激活描述符（`component::isolated::prepare`）。
    let _ = unsafe { trampoline_enter(ctx) };
    match ctx.result {
        0 => Outcome::Returned(ctx.a0_result),
        _ => Outcome::Faulted,
    }
}

/// Core trap 路径的放弃出口：恢复 `ctx` 调用者现场 + 调用者 `satp`，并交回
/// 进入点（`isolated::enter` 会返回 `Outcome::Faulted`）。
///
/// 运行在**安全 trap 栈**上（当前 AS = 被打断的实例 AS）：先恢复 trap 栈的
/// `sscratch` 约定（外层 trap 已被放弃，不会 `sret` 回来），再由汇编恢复现场。
///
/// # Safety
///
/// `ctx` 必须是一次仍然挂起的 [`enter`] 调用的上下文；调用后本执行流永不返回。
pub unsafe fn return_to_core(ctx: *mut Context) -> ! {
    trap::install_scratch_convention();
    // SAFETY: 调用方（Core 异常钩子）持有仍然挂起的 ctx。
    unsafe { trampoline_return(ctx) }
}
