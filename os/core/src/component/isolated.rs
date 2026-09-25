//! Isolated 域激活网关（**Core 侧准备**）。
//!
//! [`arch::riscv::gateway`]（双映射汇编切换）的 Core 侧对应物，两个阶段分开：
//!
//! 1. [`prepare`]（**持锁阶段**）：校验句柄 / 入口 / 栈，把 gateway 机制页落成
//!    实例侧同 VA → 同 PA 的映射，取一次 [`PreparedActivation`]（`Copy`、无引用），
//!    返回后**不再持任何 Core 锁**；
//! 2. [`enter`]（**无锁阶段**）：只把描述符搬给 arch 汇编（satp + 栈/上下文
//!    切换）。目标 root 生效后到 Core root 恢复前没有 Rust、没有锁、没有 Core 栈。
//!
//! 生产调用方只有 `component/isolated_lifecycle.rs`（Isolated 实例的 create /
//! destroy / service dispatch）；ArchTest 直接驱动本模块证明切换 / trap 往返 /
//! 组件故障分派。入口 `a0` .. `a3` 由 Core 解释（见 [`EntryArgs`]），gateway 只搬运。
//!
//! # 协作式边界（不伪造安全承诺）
//!
//! S-mode 组件与 Core 同特权级：它可以直接改 `satp` / `stvec` / 自己的映射。本
//! 模块证明的是**机制**（真页表、真 trap 往返、真恢复 / 放弃路径），不是对抗
//! 隔离；真正的强制边界是 U-mode（SandboxedNative，未实现）。ASID 恒 0 + 全量
//! `sfence.vma`。
//!
//! # 故障分派（窄 Core 钩子）
//!
//! arch 只做 eligibility：trap 来自 gateway 入口且相位是"组件运行中"才交给
//! [`on_component_fault`]。Core 侧再按显式注册的 [`FaultPolicy`] 决定：**没有
//! 策略 = `Abandon`**——组件身份本身不构成"可恢复"的证明。策略可以检查现场
//! （cause / stval / sepc），也可以经 Core API 补映射后 `Resume`。

use crate::memory::address_space::{
    self, AddressSpaceHandle, MapError, PreparedActivation, VirtualRange,
};
use arch::riscv::gateway::{self, Transition};
use arch::riscv::trap::TrapFrame;
use core::sync::atomic::{AtomicUsize, Ordering};

pub use crate::memory::address_space::IsolatedPrepareError;
pub use arch::riscv::gateway::{FaultDecision, Outcome};

/// 一次组件入口调用要交付的 `a0` .. `a3`（**入口 ABI 由 Core 解释**，arch 只搬运）。
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

/// 一次已准备、**无锁、无引用**的私有 AS 切换。
///
/// 只能由 [`prepare`] 构造；`Copy`，可以在释放锁之后再传给 [`enter`]。
/// 它携带激活描述符（backend 预打包 satp）与 Core 已验证的入口 / 栈 / slot。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedTransition {
    handle: AddressSpaceHandle,
    activation: PreparedActivation<address_space::ActiveActivation>,
    entry: usize,
    stack_top: usize,
    runtime_slot: usize,
    interrupts_enabled: bool,
    fault_token: usize,
    entry_args: EntryArgs,
}

impl PreparedTransition {
    /// 被准备的空间身份（诊断 / trace；切换汇编不解释它）。
    pub fn handle(&self) -> AddressSpaceHandle {
        self.handle
    }

    /// 目标实例 root 的预打包 satp（诊断 / 断言；切换汇编直接消费）。
    pub fn satp(&self) -> usize {
        self.activation.token().satp()
    }

    /// 组件入口 VA（Core 已验证在可执行映射内）。
    pub fn entry(&self) -> usize {
        self.entry
    }

    /// 组件栈顶 VA（Core 已验证被可写映射覆盖且 16 字节对齐）。
    pub fn stack_top(&self) -> usize {
        self.stack_top
    }
}

/// 准备一次私有 AS 切换（**锁内完成，返回时锁已释放**）。
///
/// - `stack`：组件栈的已映射区间；栈顶 = `base + size`，必须 16 字节对齐；
/// - `runtime_slot`：组件运行的 `tp`（0 = 无；只搬运，不解释）；
/// - `interrupts_enabled`：组件初始 `sstatus.SIE`（timer 往返需要它开闸；
///   service dispatch 传 `false`——与同域 service 边界同一纪律，provider 不可抢占）；
/// - `entry_args`：组件入口的 `a0` .. `a3`（入口 ABI 由 Core 解释，arch 只搬运）。
///
/// `handle` 的 owner 会作为故障归因 token 随描述符携带（Core 真相：AS owner）。
pub fn prepare(
    handle: AddressSpaceHandle,
    entry: usize,
    stack: VirtualRange,
    runtime_slot: usize,
    interrupts_enabled: bool,
    entry_args: EntryArgs,
) -> Result<PreparedTransition, IsolatedPrepareError> {
    let pages = gateway::pages();
    let activation = address_space::prepare_transition(handle, &pages, entry, stack)?;
    let owner = address_space::owner(handle).map_err(|error| match error {
        MapError::NoSuchSpace => IsolatedPrepareError::NoSuchSpace,
        MapError::Retired => IsolatedPrepareError::Retired,
        _ => IsolatedPrepareError::Unsupported,
    })?;
    // `prepare_transition` 已校验非空 + 不溢出；这里只做无需重复检查的算术。
    let stack_top = stack.base + stack.size;
    Ok(PreparedTransition {
        handle,
        activation,
        entry,
        stack_top,
        runtime_slot,
        interrupts_enabled,
        fault_token: owner.raw() as usize,
        entry_args,
    })
}

/// 准备一次**共享 Core 映射**模型下的私有 AS 切换。
///
/// 与 [`prepare`] 的唯一区别：**不**把双映射 gateway 机制页落进实例 AS——
/// Core 代码 / 栈 / 全局状态已经作为共享 Core 映射在每个 Isolated AS 里
/// same VA → same PA（见 `memory/kernel_mappings.rs`），切换只需要激活描述符。
pub fn prepare_shared(
    handle: AddressSpaceHandle,
    entry: usize,
    stack: VirtualRange,
    runtime_slot: usize,
    interrupts_enabled: bool,
    entry_args: EntryArgs,
) -> Result<PreparedTransition, IsolatedPrepareError> {
    let stack_top = stack
        .base
        .checked_add(stack.size)
        .ok_or(IsolatedPrepareError::InvalidStack)?;
    if stack.size == 0
        || stack_top % 16 != 0
        || !stack.base.is_multiple_of(crate::memory::ALLOC_GRANULE)
        || !stack.size.is_multiple_of(crate::memory::ALLOC_GRANULE)
    {
        return Err(IsolatedPrepareError::InvalidStack);
    }
    if !address_space::entry_is_executable(handle, entry)
        .map_err(|_| IsolatedPrepareError::NoSuchSpace)?
    {
        return Err(IsolatedPrepareError::EntryNotExecutable);
    }
    if !address_space::range_is_writable(handle, &stack)
        .map_err(|_| IsolatedPrepareError::NoSuchSpace)?
    {
        return Err(IsolatedPrepareError::StackNotWritable);
    }
    let activation = address_space::prepare_activation(handle).map_err(|error| match error {
        MapError::NoSuchSpace => IsolatedPrepareError::NoSuchSpace,
        MapError::Retired => IsolatedPrepareError::Retired,
        _ => IsolatedPrepareError::Unsupported,
    })?;
    let owner = address_space::owner(handle).map_err(|error| match error {
        MapError::NoSuchSpace => IsolatedPrepareError::NoSuchSpace,
        MapError::Retired => IsolatedPrepareError::Retired,
        _ => IsolatedPrepareError::Unsupported,
    })?;
    Ok(PreparedTransition {
        handle,
        activation,
        entry,
        stack_top,
        runtime_slot,
        interrupts_enabled,
        fault_token: owner.raw() as usize,
        entry_args,
    })
}

/// 执行一次私有 AS 切换：组件运行在 `transition` 的 root 上，正常返回或由 Core
/// 判为不可恢复后回到本调用者。
///
/// **不得持有任何 Core 锁**：目标 root 生效后 Core Rust 不再执行，直到 Core root
/// 恢复。中断由 arch 侧在切换前后屏蔽 / 恢复。
pub fn enter(transition: PreparedTransition) -> Outcome {
    gateway::enter(Transition {
        activation: transition.activation.token(),
        entry: transition.entry,
        stack_top: transition.stack_top,
        runtime_slot: transition.runtime_slot,
        interrupts_enabled: transition.interrupts_enabled,
        fault_token: transition.fault_token,
        arg0: transition.entry_args.a0,
        arg1: transition.entry_args.a1,
        arg2: transition.entry_args.a2,
        arg3: transition.entry_args.a3,
    })
}

/// 一次组件上下文故障的报告（交给 [`FaultPolicy`] 的现场）。
///
/// `frame` 是 arch 保存的完整被打断现场：策略可以读 `epc` / `status`，也可以
/// 修改它（例如推进 `epc` 跳过已处理的故障指令）后再 `Resume`。
pub struct ComponentFault<'a> {
    pub frame: &'a mut TrapFrame,
    /// `scause` 原值（中断位已由 arch 分离；这里是异常 cause code）。
    pub cause: usize,
    /// `stval`（故障地址 / 指令）。
    pub stval: usize,
    /// 故障归因 token = 该实例 AS 的 owner raw id（Core 真相，arch 透传）。
    pub token: usize,
}

/// Core 的窄故障策略：**组件身份本身不构成"可恢复"的证明**，恢复必须由策略
/// 显式决定。
pub type FaultPolicy = fn(&mut ComponentFault<'_>) -> FaultDecision;

static FAULT_POLICY: AtomicUsize = AtomicUsize::new(0);

/// 注册组件故障策略（一次性；重复注册返回 `false` 且不覆盖已有策略）。
pub fn register_fault_policy(policy: FaultPolicy) -> bool {
    FAULT_POLICY
        .compare_exchange(0, policy as usize, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// 把 [`on_component_fault`] 接上 arch 的 gateway trap 分派，并把
/// [`on_exception`] 接上**普通** Core trap 路径的异常钩子。
///
/// `isolated_lifecycle` 与 ArchTest 各自接线；未接线时普通异常保持 fatal。
pub fn install() {
    gateway::register_component_fault_handler(on_component_fault);
    arch::riscv::trap::register_exception_hook(on_exception);
}

/// 普通 Core trap 路径上的异常判决（Core 钩子）。
///
/// 组件在 Isolated AS 里运行时，异常走普通 `trap_vec`（Core 映射在每个实例 AS
/// 里相同，Core trap 栈也共享），因此归属必须在这里做：只有存在**匹配的
/// 可恢复上下文**时才能恢复；否则一律 fatal（返回 `false` = panic）。
///
/// 当前没有跨 AS 可恢复上下文（Isolated 的 create / destroy / service 边界
/// 仍由 gateway 的专用 trap 路径收敛），因此拒绝一切异常：保持"组件身份本身
/// 不是可恢复的证明"这一默认。
fn on_exception(_frame: *mut TrapFrame, _cause: usize, _stval: usize) -> bool {
    false
}

/// arch 交给 Core 的组件故障入口（窄钩子；见模块文档）。
fn on_component_fault(
    frame: *mut TrapFrame,
    cause: usize,
    stval: usize,
    token: usize,
) -> FaultDecision {
    let address = FAULT_POLICY.load(Ordering::Acquire);
    if address == 0 {
        // 没有显式策略：不恢复。这是"组件身份不足以证明可恢复"的默认落点。
        return FaultDecision::Abandon;
    }
    // SAFETY: 注册方保证签名与 `FaultPolicy` 一致（单一注册入口）。
    let policy: FaultPolicy = unsafe { core::mem::transmute(address) };
    // SAFETY: arch 只在本 trap 的 scratch 页（单 CPU、串行）里调用本钩子；
    // `frame` 指向其中有效的 `TrapFrame`，决定返回前不会被其他执行触碰。
    let mut fault = ComponentFault {
        frame: unsafe { &mut *frame },
        cause,
        stval,
        token,
    };
    policy(&mut fault)
}

// 说明：本模块只在 RISC-V + S-mode + MMU 目标编译（依赖 `arch::riscv::gateway`
// 与 `TrapFrame`），host 测试无法编译它。这里唯一的策略不变量——**没有显式策略
// 就是 `Abandon`，绝不因"来自组件"而恢复**——由 `on_component_fault` 的早退分支
// 直接保证，并由 QEMU ArchTest `isolated-fault-abandon` 端到端验证（组件上下文
// 故障 → Core 钩子 → 拒绝恢复 → `Outcome::Faulted`）。
