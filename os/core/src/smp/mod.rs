//! SMP 骨架：BSP/AP 编排、每 CPU 记录、启动屏障、pending IPI。
//!
//! # 边界（对齐 `AGENTS.md`）
//!
//! - **Core owns truth**：逻辑 CPU id、启动状态、pending work、调度归属、
//!   启动屏障都在这里。arch 只提供“绑定本 CPU / 初始化本地机制 / 启动硬件
//!   CPU / 响门铃”的机制（`arch::Smp`）。
//! - **策略不在 Core**：不做运行队列、负载均衡、调度算法、跨 CPU RPC。
//! - 逻辑 `CpuId` 与硬件 `arch::cpu::HardwareCpuId` 分离：真实 CPU 数一律来自
//!   启动后发现的 `MachineInfo`（`cpu_count`），编译期容量只有一处
//!   [`crate::machine::MAX_CPUS`]。
//!
//! # 骨架状态
//!
//! 纯数据结构（[`CpuMask`] / [`PerCpu`] / [`CpuBootState`] / [`BootGate`]）已实现
//! 并有 host 单测；**SMP 编排与硬件路径**（[`init`] / [`secondary_entry`] /
//! [`current_cpu`] / IPI 投递）的方法体为 `todo!()`，由人类手写。
//!
//! 本模块的 `todo!()` **不会**被默认（单 CPU）构建路径调用，因此现有 host /
//! rv64 / rv32 测试保持全绿；SMP 真正的接通属于实现阶段。

#![allow(dead_code)] // SMP 骨架：接口与类型先立住，默认构建路径尚未调用它们

mod boot;
mod ipi;
mod mask;
mod percpu;

pub use boot::{BootGate, CpuBootState};
pub use ipi::{IpiRequest, NotifyError};
pub use mask::{CpuIndexError, CpuMask, CpuMaskIter};
pub use percpu::{PerCpu, PerCpuError};

use crate::machine::{CpuId, MachineInfo};
use arch::cpu::HardwareCpuId;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicUsize};

/// 当前编译目标的 SMP 后端（CPU 启动 + IPI 传输）。
pub type Backend = arch::SmpImpl;

/// [`init`] 的失败原因。失败即 **fail-closed**：已经被请求启动的 AP 全部
/// 保持 parked，不发明“部分成功”策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmpInitError {
    /// 发现的拓扑不合法（无 CPU、boot hart 缺失、重复逻辑身份等）。
    InvalidTopology,
    /// per-CPU 记录 / 本地存储分配失败。
    AllocationFailed,
    /// 后端 `prepare` 失败（CLINT/ACPI/PSCI/mailbox 等）。
    BackendInit(arch::smp::InitError),
    /// 某个 AP 的启动请求被拒绝。
    StartRejected {
        cpu: CpuId,
        reason: arch::smp::CpuStartError,
    },
    /// 等待 AP 进入 Ready/Online 超时。**超时不证明 AP 不会迟到进入**：
    /// 描述符、栈、本地存储都必须保留，不自动回收、不重试。
    Timeout { cpu: CpuId },
}

/// 每个逻辑 CPU 的真相记录（Core 拥有）。
///
/// **不**包含调度/中断的完整本地状态字段——那些在 per-CPU 重塑（见
/// `docs/modules/arch.md` 的 SMP 章节）落地后补入；这里先立住身份、启动状态与
/// pending IPI 三样 Core 独占的真相。
pub(crate) struct CpuRecord {
    /// 硬件身份，由 discovery 建立映射；Core 不假设 BSP = 0。
    hardware_id: HardwareCpuId,
    /// [`CpuBootState`] 的原子编码。
    boot_state: AtomicU8,
    /// 待处理 IPI work 位集（Core 语义，不是 arch 的硬件寄存器）。
    pending_ipi: AtomicUsize,
    /// 已发布的本地存储地址；远端 CPU **不得**解引用，只作发布/查询。
    local: AtomicPtr<CpuLocal>,
}

/// 严格 CPU-local 的状态（contamination / IRQ 嵌套等）。
///
/// 骨架先留空壳；其内部类型（`CpuContainment` / `CpuIrqState`）在对应模块
/// 暴露 per-CPU 结构后补入。**它不是组件 runtime slot**。
pub(crate) struct CpuLocal {
    _reserved: (),
}

/// BSP 启动流程：校验拓扑 → 发布记录与启动描述符 → 绑定自身 → 注册全局回调
/// → 逐个请求 AP → 等全部 Ready → 放行屏障 → 等 Online。任一 AP 失败即 fail-closed。
pub fn init(_machine: &MachineInfo) -> Result<(), SmpInitError> {
    todo!("SMP: BSP bring-up (validate topology, publish records, start APs, boot gate)")
}

/// 当前执行 CPU 的逻辑身份。仅在该 CPU 完成本地绑定后有效。
pub fn current_cpu() -> CpuId {
    todo!("SMP: resolve the logical CpuId from the arch CPU-local entry record")
}

/// 已 Online 的逻辑 CPU 集合。
pub fn online_cpus() -> CpuMask {
    todo!("SMP: return the Core-owned online CPU set")
}

/// 查询某个逻辑 CPU 的启动状态。
pub fn cpu_state(_cpu: CpuId) -> Result<CpuBootState, CpuIndexError> {
    todo!("SMP: read the CPU record's boot state")
}

/// AP 入口：由 arch 启动 trampoline 进入，每个 AP 一次。
///
/// `argument` = Core 校验过的逻辑 CPU 下标。
///
/// # Safety
/// 只能由 arch 启动 trampoline 进入，且满足文档要求的执行环境（地址空间、栈、
/// ABI、关中断、runtime slot = 0）。
pub unsafe extern "C" fn secondary_entry(_argument: usize) -> ! {
    todo!("SMP: AP entry (bind local storage, init cpu/controller/timer/ipi, wait gate, go online)")
}

fn mark_ready(_cpu: CpuId) -> Result<(), SmpInitError> {
    todo!("SMP: transition an AP Starting -> Ready")
}

fn wait_for_release(_cpu: CpuId) -> Result<(), SmpInitError> {
    todo!("SMP: AP waits on the boot gate with interrupts disabled")
}

fn release_secondaries() -> Result<(), SmpInitError> {
    todo!("SMP: release the boot gate so every Ready AP may proceed to Online")
}

fn wait_until_online(_cpus: &CpuMask, _deadline: u64) -> Result<(), SmpInitError> {
    todo!("SMP: BSP waits for the requested APs to reach Online")
}
