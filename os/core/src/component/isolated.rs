//! Isolated 域激活（**Core 侧准备**）：私有 AS 的入口 / 栈校验、激活描述符与
//! 最小跨 AS 执行原语 [`arch::riscv::trampoline`] 的绑定。
//!
//! ```text
//! prepare（持锁阶段）：校验句柄 / 入口（私有或共享可执行映射）/ 栈，
//!                     取一次 PreparedActivation（Copy、无引用），返回后不持锁；
//! enter（无锁阶段）：组装 per-invocation 的 trampoline Context → 进入目标 AS。
//! ```
//!
//! # 调用模型（共享 Core 映射）
//!
//! Core 代码 / 栈 / 全局状态在每个 Isolated AS 里 same VA → same PA
//! （`memory/kernel_mappings.rs`），`stvec` 恒为 `trap::vector_address()`：
//!
//! - Isolated → Core：普通 C-ABI 调用，`satp` 保持实例 root，零切换；
//! - Isolated 内的 trap：走**普通** Core trap 路径（安全 trap 栈 + 完整
//!   `TrapFrame`），`satp` 全程等于实例 root；
//! - 故障归因由 [`on_exception`]（Core 的异常钩子）完成：只有存在**匹配的
//!   可恢复现场**（[`containment::CrossAsContext`]：活动 satp 匹配、非 Core
//!   代码、非嵌套处理、非 IRQ scope）才允许策略决定；默认 **拒绝恢复**。
//! - 放弃：`CrossAsContext::abandon` → `trampoline::return_to_core` → 回到
//!   `enter` 的调用者（`Outcome::Faulted`），由调用方的边界做清理。
//!
//! # 协作式边界（不伪造安全承诺）
//!
//! S-mode 组件与 Core 同特权级：它可以直接改 `satp` / `stvec` / 自己的映射。
//! 本模块证明的是**机制**（真页表、真 trap 往返、真恢复 / 放弃路径），不是对抗
//! 隔离；RV64 U-mode Sandboxed 的强制边界由 sandbox 模块提供。ASID 恒 0 + 全量
//! `sfence.vma`。

use crate::component::containment::{self, cross_as::CrossAsContext};
use crate::memory::address_space::{self, AddressSpaceHandle, PreparedActivation, VirtualRange};
use arch::riscv::trampoline::{self, Transition};
use arch::riscv::trap::TrapFrame;
use core::sync::atomic::{AtomicUsize, Ordering};

pub use crate::memory::address_space::IsolatedPrepareError;
pub use arch::riscv::trampoline::Outcome;

/// 组件上下文故障的分派决定（由 Core 注册的策略返回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum FaultDecision {
    /// 不可恢复：交回进入前的 Core 调用者（[`Outcome::Faulted`]）。
    Abandon = 0,
    /// 可恢复：按（可能被策略修改过的）trap 帧恢复组件并 `sret`。
    Resume = 1,
}

/// 一次组件入口调用要交付的 `a0` .. `a3`（**入口 ABI 由 Core 解释**）。
///
/// - create：`a0 = args`、`a1 = out_state`（`a2` / `a3` = 0）；
/// - destroy：`a0 = state`（`a1` .. `a3` = 0）；
/// - service dispatch：`a0 = instance_state`、`a1 = port`、`a2 = method`、
///   `a3 = frame`（provider 域内 VA）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryArgs {
    pub a0: usize,
    pub a1: usize,
    pub a2: usize,
    pub a3: usize,
}

impl EntryArgs {
    /// 两个参数的入口（create / destroy）。
    pub const fn pair(a0: usize, a1: usize) -> Self {
        Self {
            a0,
            a1,
            a2: 0,
            a3: 0,
        }
    }
}

/// 一次已准备、**无锁、无引用**的私有 AS 进入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedTransition {
    handle: AddressSpaceHandle,
    activation: PreparedActivation<address_space::ActiveActivation>,
    entry: usize,
    stack_top: usize,
    interrupts_enabled: bool,
    entry_args: EntryArgs,
}

impl PreparedTransition {
    /// 被准备的空间身份（诊断 / trace；切换汇编不解释它）。
    pub fn handle(&self) -> AddressSpaceHandle {
        self.handle
    }

    /// 目标实例 root 的预打包 satp（诊断 / 断言）。
    pub fn satp(&self) -> usize {
        self.activation.token().satp()
    }
}

/// 准备一次私有 AS 进入（**锁内完成，返回时锁已释放**）。
///
/// - `stack`：组件栈的已映射区间；栈顶 = `base + size`，必须 16 字节对齐；
/// - `interrupts_enabled`：组件初始 `sstatus.SIE`（timer 往返需要它开闸；
///   service dispatch 传 `false`——与同域 service 边界同一纪律）；
/// - `entry_args`：组件入口的 `a0` .. `a3`（入口 ABI 由 Core 解释）。
pub fn prepare(
    handle: AddressSpaceHandle,
    entry: usize,
    stack: VirtualRange,
    interrupts_enabled: bool,
    entry_args: EntryArgs,
) -> Result<PreparedTransition, IsolatedPrepareError> {
    let activation = address_space::prepare_transition(handle, entry, stack)?;
    // `prepare_transition` 已校验非空 + 不溢出；这里只做算术。
    let stack_top = stack.base + stack.size;
    Ok(PreparedTransition {
        handle,
        activation,
        entry,
        stack_top,
        interrupts_enabled,
        entry_args,
    })
}

/// 执行一次私有 AS 进入：组件运行在目标 root 上，正常返回或由 Core trap 路径
/// 判为不可恢复后回到本调用者。
///
/// 进入期间安装 per-invocation 的 [`CrossAsContext`]：普通 trap 路径的异常
/// 钩子据此归因 / 恢复 / 放弃。未来 A→B 嵌套时 ctx 的 LIFO 链天然正确。
pub fn enter(transition: PreparedTransition) -> Outcome {
    assert!(
        transition.entry != 0 && transition.stack_top.is_multiple_of(16),
        "isolated transition is not prepared: entry={:#x} stack_top={:#x}",
        transition.entry,
        transition.stack_top
    );

    let mut context = trampoline::Context::new(Transition {
        activation: transition.activation.token(),
        entry: transition.entry,
        stack_top: transition.stack_top,
        interrupts_enabled: transition.interrupts_enabled,
        arg0: transition.entry_args.a0,
        arg1: transition.entry_args.a1,
        arg2: transition.entry_args.a2,
        arg3: transition.entry_args.a3,
    });
    let mut cross = CrossAsContext {
        context: core::ptr::addr_of_mut!(context).cast::<u8>(),
        abandon: abandon_cross_as,
        expected_satp: context.instance_satp(),
        space: Some(transition.handle),
    };

    // 安装可恢复现场（LIFO；返回 / 放弃后恢复上一个）。
    let previous = containment::cross_as::swap_cross_as(core::ptr::addr_of_mut!(cross));
    let outcome = trampoline::enter(&mut context);
    containment::cross_as::restore_cross_as(previous);
    // Faulted：Core trap 路径已把控制权交回这里；清理由调用方边界完成。
    outcome
}

/// `CrossAsContext::abandon` 的实现：放弃被打断的执行，交回挂起的 Core 调用者。
///
/// # Safety
///
/// `context` 必须是一次仍然挂起的 `enter` 的 trampoline 记录。
unsafe fn abandon_cross_as(context: *mut u8) -> ! {
    // SAFETY: 调用方（Core 异常钩子）保证该指针来自仍然挂起的 enter。
    unsafe { trampoline::return_to_core(context.cast::<trampoline::Context>()) }
}

/// 一次组件上下文异常的报告（交给 [`FaultPolicy`] 的现场）。
pub struct ComponentFault<'a> {
    pub frame: &'a mut TrapFrame,
    /// `scause` 原值（中断位已由 arch 分离；这里是异常 cause code）。
    pub cause: usize,
    /// `stval`（故障地址 / 指令）。
    pub stval: usize,
}

/// Core 的窄故障策略：**组件身份本身不构成"可恢复"的证明**，恢复必须由策略
/// 显式决定；没有策略 = [`FaultDecision::Abandon`]。
pub type FaultPolicy = fn(&mut ComponentFault<'_>) -> FaultDecision;

static FAULT_POLICY: AtomicUsize = AtomicUsize::new(0);

/// 注册组件故障策略（一次性；重复注册返回 `false` 且不覆盖已有策略）。
pub fn register_fault_policy(policy: FaultPolicy) -> bool {
    FAULT_POLICY
        .compare_exchange(0, policy as usize, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// 把 [`on_exception`] 接上**普通** Core trap 路径的异常钩子。
///
/// `isolated_lifecycle` 与 ArchTest 各自接线；未接线时异常保持 fatal。
pub fn install() {
    arch::riscv::trap::register_exception_hook(on_exception);
}

/// 普通 Core trap 路径上的异常判决（Core 异常钩子）。
///
/// 只有满足**全部**条件才允许策略介入；否则返回 `false` = fatal：
///
/// 1. 存在活动的跨 AS 可恢复现场（[`containment::active_cross_as`]）；
/// 2. 被打断执行的 `satp` 就是该现场预期的实例 root（不是别的 AS / Core）；
/// 3. 故障 PC **不在共享 Core 可执行映射**里（那是 Core 代码 / trampoline：
///    Core bug 而不是组件故障）——未映射取指（如 abandon 探针）不算 Core；
/// 4. 不是嵌套处理（被打断的 sp 不在安全 trap 栈上）；
/// 5. 不处于 Core-critical 深度（[`containment::core_abi_depth`]）；
/// 6. active chain 里没有 IRQ attribution scope。
///
/// 通过后：策略缺失 = `Abandon`；`Resume` = 按（可能被修改的）帧恢复组件；
/// `Abandon` = 经 [`CrossAsContext::abandon`] 交回进入前的 Core 调用者。
fn on_exception(frame: *mut TrapFrame, cause: usize, stval: usize) -> bool {
    #[cfg(target_arch = "riscv64")]
    if unsafe { super::sandbox::on_trap(frame, cause, stval) } {
        return true;
    }
    #[cfg(target_arch = "riscv64")]
    if unsafe { crate::task::user::on_trap(frame, cause, stval) } {
        return true;
    }
    let Some(cross_ptr) = containment::cross_as::active_cross_as() else {
        return false;
    };
    // SAFETY: 现场由 `isolated::enter` 的栈帧持有，trap 期间仍挂起。
    let cross = unsafe { &*cross_ptr };
    if arch::riscv::mmu::current_satp() != cross.expected_satp {
        return false;
    }
    // SAFETY: `frame` 指向 arch 在本 trap 中保存在安全 trap 栈上的有效帧。
    let frame_ref = unsafe { &mut *frame };
    let epc = frame_ref.epc;
    let Some(space) = cross.space else {
        return false;
    };
    if address_space::shared_executable_at(space, epc).unwrap_or(true) {
        return false;
    }
    let (stack_base, stack_top) = arch::riscv::trap::trap_stack_range();
    let interrupted_sp = frame_ref.x[2];
    if interrupted_sp >= stack_base && interrupted_sp < stack_top {
        // 嵌套处理：安全 trap 栈上的 handler 自己故障。
        return false;
    }
    if containment::core_abi_depth() > 0 || containment::irq_in_chain() {
        return false;
    }

    let address = FAULT_POLICY.load(Ordering::Acquire);
    let decision = if address == 0 {
        FaultDecision::Abandon
    } else {
        // SAFETY: 注册方保证签名与 `FaultPolicy` 一致（单一注册入口）。
        let policy: FaultPolicy = unsafe { core::mem::transmute(address) };
        let mut fault = ComponentFault {
            frame: frame_ref,
            cause,
            stval,
        };
        policy(&mut fault)
    };
    match decision {
        FaultDecision::Resume => true,
        FaultDecision::Abandon => {
            // SAFETY: 现场仍然挂起；`abandon` 永不返回本执行流。
            unsafe { (cross.abandon)(cross.context) }
        }
    }
}

// 说明：本模块只在 RISC-V + S-mode + MMU 目标编译（依赖 `arch::riscv::trampoline`
// 与 `TrapFrame`）。唯一默认不变量——**没有显式策略就是 `Abandon`，绝不因
// "来自组件"而恢复**——由 `on_exception` 的早退分支直接保证，并由 QEMU
// ArchTest `isolated-fault` / `isolated-fault-abandon` 端到端验证。
