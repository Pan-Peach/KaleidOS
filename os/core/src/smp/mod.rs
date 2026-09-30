//! SMP：BSP/AP 编排、每 CPU 记录、启动屏障、pending IPI。
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
//! # 当前接通程度（M1）
//!
//! - **已机械实现**：[`CpuRegistry`]（每 CPU 记录表 + 拓扑校验 + 启动状态机 +
//!   online 集合）、[`init`]（BSP 侧 Core 初始化）、[`cpu_state`]、
//!   [`online_cpus`]、[`request_start`]、`mark_ready`、[`record`]。
//! - **仍是 `todo!()`（人类实现，见 `.omo/plans/smp-production-integration.md`）**：
//!   [`secondary_entry`]（AP 入口编排）、`wait_for_release` / `release_secondaries`
//!   / `wait_until_online`（BootGate + 超时）、`ipi::notify` / `ipi::drain_pending`。
//!
//! # `init` 为什么**不**启动 AP
//!
//! Core `init`（`crate::init`）发生在 boot 建立**长期内核地址空间**
//! （`runtime::init`）之前；而 boot 的 AP 启动描述符要携带当时的 `satp`。因此
//! AP 的物理启动仍由 boot 在地址空间就绪后触发（`os/boot/riscv/src/smp.rs`），
//! Core 只拥有**身份 / 状态 / 就绪 / online** 这些真相。把物理启动搬进 arch
//! `Smp::start_cpu` 属后续里程碑（需人类定稿 seam）。

#![allow(dead_code)] // M1：接口与类型先立住，部分尚未被默认构建路径调用

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
use arch::smp::Smp;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering};
use spin::Once;

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
/// **不**包含调度/中断的完整本地状态字段——那些在 per-CPU 重塑落地后补入；
/// 这里先立住身份、启动状态与 pending IPI 三样 Core 独占的真相。
pub(crate) struct CpuRecord {
    /// 硬件身份，由 discovery 建立映射。
    hardware_id: HardwareCpuId,
    /// [`CpuBootState`] 的原子编码。
    boot_state: AtomicU8,
    /// 待处理 IPI work 位集（Core 语义，不是 arch 的硬件寄存器）。
    pending_ipi: AtomicUsize,
    /// 已消费 pending、等待在安全边界真正重调度的标志（由 `drain_pending` 置位）。
    resched: AtomicBool,
    /// 本地启动时是否验证过自己的逻辑身份（`current_cpu() == argument`）。
    identity_ok: AtomicBool,
    /// 已发布的本地存储地址；远端 CPU **不得**解引用，只作发布/查询。
    local: AtomicPtr<CpuLocal>,
}

impl CpuRecord {
    fn new(hardware_id: HardwareCpuId) -> Self {
        Self {
            hardware_id,
            boot_state: AtomicU8::new(CpuBootState::Offline.as_raw()),
            pending_ipi: AtomicUsize::new(0),
            resched: AtomicBool::new(false),
            identity_ok: AtomicBool::new(false),
            local: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// 置「需要重调度」标志（[`ipi::drain_pending`] 消费 pending 后调用）。
    pub(crate) fn set_resched(&self) {
        self.resched.store(true, Ordering::Release);
    }

    /// 消费「需要重调度」标志（调度安全的边界调用）。
    pub(crate) fn take_resched(&self) -> bool {
        self.resched.swap(false, Ordering::AcqRel)
    }

    /// 记录 AP 已在本 CPU 上验证过逻辑身份（`secondary_entry` 调用）。
    fn set_identity_ok(&self) {
        self.identity_ok.store(true, Ordering::Release);
    }

    /// 该 CPU 是否验证过自己的逻辑身份。
    pub(crate) fn identity_ok(&self) -> bool {
        self.identity_ok.load(Ordering::Acquire)
    }

    /// 本记录的硬件身份。
    pub(crate) fn hardware_id(&self) -> HardwareCpuId {
        self.hardware_id
    }

    /// 当前启动状态（非法编码按 `Offline` 处理，绝不 panic）。
    pub(crate) fn boot_state(&self) -> CpuBootState {
        CpuBootState::from_raw(self.boot_state.load(Ordering::Acquire))
            .unwrap_or(CpuBootState::Offline)
    }

    /// 条件状态迁移：仅当当前为 `from` 时迁移到 `to`。
    fn try_transition(&self, from: CpuBootState, to: CpuBootState) -> bool {
        self.boot_state
            .compare_exchange(
                from.as_raw(),
                to.as_raw(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn set_boot_state(&self, state: CpuBootState) {
        self.boot_state.store(state.as_raw(), Ordering::Release);
    }
}

/// 严格 CPU-local 的状态（contamination / IRQ 嵌套等）。
///
/// 骨架先留空壳；其内部类型在对应模块暴露 per-CPU 结构后补入。
/// **它不是 per-CPU 基址记录。**
pub(crate) struct CpuLocal {
    _reserved: (),
}

/// Core 拥有的 CPU 真相表：每 CPU 一条 [`CpuRecord`] + BSP 的逻辑身份。
///
/// 纯数据结构（可脱离全局单测）：`PerCpu` 按逻辑 id 稠密索引。
struct CpuRegistry {
    records: PerCpu<CpuRecord>,
    bsp: CpuId,
}

impl CpuRegistry {
    /// 从已发现的 `MachineInfo` 建立记录表并校验拓扑。不改任何状态。
    fn build(machine: &MachineInfo) -> Result<Self, SmpInitError> {
        let count = machine.cpu_count;
        if count == 0 || count > machine.cpu_info.len() {
            return Err(SmpInitError::InvalidTopology);
        }
        let cpus = &machine.cpu_info[..count];

        // 硬件身份不得重复：重复会把两个逻辑 CPU 指向同一物理 hart（谓词式发现
        // 的一致性由 Core 守住）。
        for (i, a) in cpus.iter().enumerate() {
            for b in &cpus[i + 1..] {
                if a.hardware_id == b.hardware_id {
                    return Err(SmpInitError::InvalidTopology);
                }
            }
        }

        // BSP：优先 `boot_cpu` 标记，其次按 `boot_hardware_id` 匹配。
        // 归一化（boot hart = 逻辑 0）由 boot 完成；这里只忠实反映发现结果。
        let bsp = cpus
            .iter()
            .position(|c| c.boot_cpu)
            .or_else(|| {
                cpus.iter()
                    .position(|c| c.hardware_id == machine.boot_hardware_id)
            })
            .ok_or(SmpInitError::InvalidTopology)?;

        // 不变式（boot 归一化保证）：BSP 必须是逻辑 CPU0。入口记录、`trap_stack_range()`、
        // PLIC 外部固定路由都依赖它；Core 在这里守住，防止 boot 漂移后静默错绑。
        if bsp != 0 {
            return Err(SmpInitError::InvalidTopology);
        }

        let records = PerCpu::new(count, |cpu| CpuRecord::new(cpus[cpu.raw()].hardware_id))
            .map_err(|_| SmpInitError::AllocationFailed)?;
        Ok(Self {
            records,
            bsp: CpuId::from_raw(bsp),
        })
    }

    fn bsp(&self) -> CpuId {
        self.bsp
    }

    fn state(&self, cpu: CpuId) -> Result<CpuBootState, CpuIndexError> {
        self.records
            .get(cpu)
            .map(|record| record.boot_state())
            .ok_or(CpuIndexError::OutOfRange)
    }

    /// 已 Online 的逻辑 CPU 集合（扫描真相，不另立副本）。
    fn online(&self) -> CpuMask {
        let mut mask = CpuMask::empty();
        for (cpu, record) in self.records.iter() {
            if record.boot_state() == CpuBootState::Online {
                let _ = mask.insert(cpu);
            }
        }
        mask
    }

    fn hardware_id(&self, cpu: CpuId) -> Option<HardwareCpuId> {
        self.records.get(cpu).map(CpuRecord::hardware_id)
    }

    /// 请求启动某个 AP（`Offline → Starting`）。幂等：已 Starting 及以上视为已请求。
    /// 人类实现的 `secondary_entry` / boot 编排在请求硬件启动**之前**调用。
    fn request_start(&self, cpu: CpuId) -> Result<(), SmpInitError> {
        let record = self.records.get(cpu).ok_or(SmpInitError::InvalidTopology)?;
        match record.boot_state() {
            CpuBootState::Offline => {
                let _ = record.try_transition(CpuBootState::Offline, CpuBootState::Starting);
                Ok(())
            }
            CpuBootState::Starting | CpuBootState::Ready | CpuBootState::Online => Ok(()),
            // Failed 是终态：迟到的启动请求不得复活它。
            CpuBootState::Failed => Err(SmpInitError::InvalidTopology),
        }
    }

    /// AP 完成本地初始化后置 Ready（`Starting → Ready`）。幂等。
    fn mark_ready(&self, cpu: CpuId) -> Result<(), SmpInitError> {
        let record = self.records.get(cpu).ok_or(SmpInitError::InvalidTopology)?;
        match record.boot_state() {
            CpuBootState::Starting => {
                let _ = record.try_transition(CpuBootState::Starting, CpuBootState::Ready);
                Ok(())
            }
            CpuBootState::Ready | CpuBootState::Online => Ok(()),
            CpuBootState::Offline | CpuBootState::Failed => Err(SmpInitError::InvalidTopology),
        }
    }

    /// BSP 发布：`init` 在 BSP 上直接置 Online（BSP 不走 AP 入口）。
    fn publish_bsp_online(&self, cpu: CpuId) -> Result<(), SmpInitError> {
        let record = self.records.get(cpu).ok_or(SmpInitError::InvalidTopology)?;
        record.set_boot_state(CpuBootState::Online);
        Ok(())
    }

    /// AP 置 Online：只允许 `Ready → Online`（fail-closed；Failed 终态不可复活）。
    fn mark_online(&self, cpu: CpuId) -> Result<(), SmpInitError> {
        let record = self.records.get(cpu).ok_or(SmpInitError::InvalidTopology)?;
        if record.try_transition(CpuBootState::Ready, CpuBootState::Online)
            || record.boot_state() == CpuBootState::Online
        {
            Ok(())
        } else {
            Err(SmpInitError::InvalidTopology)
        }
    }

    /// 标记失败（终态；迟到入口不得复活）。
    fn mark_failed(&self, cpu: CpuId) {
        if let Some(record) = self.records.get(cpu) {
            record.set_boot_state(CpuBootState::Failed);
        }
    }
}

/// 全局 Core CPU 真相表（`init` 发布一次）。
static RECORDS: Once<CpuRegistry> = Once::new();

/// BSP ↔ AP 启动屏障（Core 拥有）：AP 在 Ready 后自旋等待，BSP 在全部就绪后放行。
static GATE: BootGate = BootGate::new();

/// 启动期记录的 timebase 频率，用于 AP bring-up 超时（0 = 未知）。
static TIMEBASE_HZ: AtomicUsize = AtomicUsize::new(0);

/// AP bring-up 超时的兜底 tick 上限（timebase 未知时使用）。
const AP_BOOT_TIMEOUT_FALLBACK: u64 = 20_000_000;

/// BSP 启动流程（M1，机械）：校验拓扑 → 发布记录 → 注册 Core IPI 回调 →
/// 置 BSP Online。
///
/// **不启动 AP**（见模块头）：AP 的物理启动仍由 boot 在长期地址空间就绪后触发。
/// **不** `enable_ipi_interrupt`：接收端 SSIP ack 与 `drain_pending` 尚未落地，
/// 此刻开源会造成中断风暴。失败即 fail-closed（不发布半成品记录）。
pub fn init(machine: &MachineInfo) -> Result<(), SmpInitError> {
    let registry = CpuRegistry::build(machine)?;
    let bsp = registry.bsp();

    TIMEBASE_HZ.store(machine.timebase_frequency as usize, Ordering::Release);

    // Core IPI 回调必须在**任何 CPU 打开 IPI 接收之前**注册（覆盖语义）。
    // 只在真有多 CPU 时触碰后端：UP 无 IPI，也让 host/单核 profile 免于后端差异。
    if machine.cpu_count > 1 {
        <Backend as Smp>::register_ipi_handler(ipi::ipi_interrupt)
            .map_err(SmpInitError::BackendInit)?;
        <Backend as Smp>::init_cpu().map_err(SmpInitError::BackendInit)?;
    }

    registry.publish_bsp_online(bsp)?;
    RECORDS.call_once(|| registry);
    Ok(())
}

/// 当前执行 CPU 的逻辑身份。由 arch 的 CPU-local 入口记录解析（UP 恒为 CPU0）。
pub fn current_cpu() -> CpuId {
    <arch::CpuImpl as arch::CpuArch>::current_cpu()
        .expect("current CPU is not bound (install_per_cpu_base was not called)")
}

/// 已 Online 的逻辑 CPU 集合。`init` 之前为空集。
pub fn online_cpus() -> CpuMask {
    RECORDS
        .get()
        .map_or_else(CpuMask::empty, CpuRegistry::online)
}

/// 查询某个逻辑 CPU 的启动状态；未发布的 CPU 返回 [`CpuIndexError::OutOfRange`]。
pub fn cpu_state(cpu: CpuId) -> Result<CpuBootState, CpuIndexError> {
    match RECORDS.get() {
        Some(registry) => registry.state(cpu),
        None => Err(CpuIndexError::OutOfRange),
    }
}

/// 某逻辑 CPU 的 Canonical 记录（Core 内部消费者：ipi / sched / timer）。
///
/// 只发布**只读**引用；远端 CPU 不得解引用记录里的宿主指针（见 [`CpuRecord::local`]）。
pub(crate) fn record(cpu: CpuId) -> Option<&'static CpuRecord> {
    RECORDS.get().and_then(|registry| registry.records.get(cpu))
}

/// 请求启动某个 AP（Core 真相：`Offline → Starting`）。见 [`CpuRegistry::request_start`]。
///
/// boot 在请求硬件启动某个 AP **之前**调用；`pub` 供 boot crate 使用。
pub fn request_start(cpu: CpuId) -> Result<(), SmpInitError> {
    RECORDS
        .get()
        .ok_or(SmpInitError::InvalidTopology)?
        .request_start(cpu)
}

/// 查询某逻辑 CPU 的硬件身份（`ipi::notify` 等需要硬件 id 响铃）。
pub(crate) fn hardware_id(cpu: CpuId) -> Option<HardwareCpuId> {
    RECORDS.get().and_then(|registry| registry.hardware_id(cpu))
}

/// 消费某逻辑 CPU 的「需要重调度」标志（调度安全边界调用；见 [`CpuRecord::take_resched`]）。
pub(crate) fn take_resched(cpu: CpuId) -> bool {
    record(cpu).is_some_and(CpuRecord::take_resched)
}

/// 该 CPU 是否在本 CPU 上验证过自己的逻辑身份（`smp-percpu` 证据）。
pub fn cpu_identity_ok(cpu: CpuId) -> bool {
    record(cpu).is_some_and(CpuRecord::identity_ok)
}

/// AP 入口：由 arch 启动 trampoline 进入，每个 AP 一次。
///
/// `argument` = Core 校验过的逻辑 CPU 下标。顺序：绑定本地入口记录 → 本 CPU 子系统
/// （sched/timer/irq/ipi）→ 验证身份 → Ready → 等 BSP 放行（BootGate）→ Online →
/// 打开 IPI 源与全局中断 → Core 空闲循环。
///
/// # Safety
/// 只能由 arch 启动 trampoline 进入，且满足文档要求的执行环境（地址空间、栈、
/// ABI、关中断、`tp = 0`）。
pub unsafe extern "C" fn secondary_entry(argument: usize) -> ! {
    let cpu = CpuId::from_raw(argument);

    // 1) 绑定本 CPU 的入口记录（`sscratch`）：trap 入口据此换栈 + 解析身份。
    //    真实 Core-owned `CpuLocal` 尚未定义，先用 dangling 占位（Core 只存 / 传）。
    // SAFETY: 由 arch 启动 trampoline 进入，仅本 CPU、关中断、online 之前。
    unsafe {
        <arch::CpuImpl as arch::CpuArch>::install_per_cpu_base(cpu, core::ptr::NonNull::dangling());
    }
    <arch::CpuImpl as arch::CpuArch>::init_cpu(); // stvec / trap 入口

    // 2) 本 CPU 的 Core 子系统。containment 尚未 per-CPU 化（plan）——本里程碑
    //    AP 不运行组件任务，故不调用 `containment::init_cpu`（保持其 `todo!()` 不被触发）。
    //    **任一本地子系统初始化失败即 fail-closed**：不得宣称 Online（Oracle 评审）。
    let init_ok = crate::sched::init_cpu(cpu).is_ok()
        && crate::timer::init_cpu(cpu).is_ok()
        && crate::irq::init_cpu(cpu).is_ok()
        && <Backend as Smp>::init_cpu().is_ok();

    // 3) 身份 + 初始化门禁：任一失败即标记 Failed 并 park，绝不进入调度。
    if !init_ok || current_cpu() != cpu {
        ap_failed(cpu);
    }
    if let Some(record) = record(cpu) {
        record.set_identity_ok();
    }

    // 4) Ready，并在 BootGate 上关中断自旋（全局 SIE 尚未开）。
    if mark_ready(cpu).is_err() || wait_for_release(cpu).is_err() {
        ap_failed(cpu);
    }

    // 5) 只允许 Ready→Online；失败即 fail-closed。成功后开 IPI 源与全局中断，进入空闲循环。
    if set_online(cpu).is_err() {
        ap_failed(cpu);
    }
    <Backend as Smp>::enable_ipi_interrupt();
    <arch::CpuImpl as arch::CpuArch>::enable_irq();
    idle_loop(cpu)
}

/// AP 启动失败（终态 `Failed`）后 park——绝不进入调度。
fn ap_failed(cpu: CpuId) -> ! {
    if let Some(record) = record(cpu) {
        record.set_boot_state(CpuBootState::Failed);
    }
    idle_loop(cpu)
}

/// CPU 的 Core 空闲循环：自旋 + `wfi`；IPI 只在硬件回调里标记 pending，在这里
/// （安全边界）drain。本里程碑 AP 无跨 CPU 任务，故不进入 `sched::run`。
fn idle_loop(cpu: CpuId) -> ! {
    loop {
        // 「检查-睡眠」原子化（Oracle 评审：防丢唤醒）：先关本 CPU 中断再 drain；
        // 此后到达的门铃让 `sip.SSIP` 保持 pending，而 SIE 关闭下的 `wfi` 会立即
        // 返回（pending 即唤醒），不会出现“drain 完就睡、门铃被吞”的永久睡眠。
        let flags = <arch::CpuImpl as arch::CpuArch>::disable_irq();
        ipi::drain_pending(cpu);
        let _ = take_resched(cpu);
        // TODO(手写)：containment per-CPU 落地后，在此进入本 CPU 的调度——
        //   if crate::sched::has_claimable_for(cpu) {
        //       let _ = crate::sched::run();
        //   }
        <arch::CpuImpl as arch::CpuArch>::wait_for_interrupt();
        <arch::CpuImpl as arch::CpuArch>::restore_irq(flags);
    }
}

/// AP 完成本地初始化后置 Ready。
fn mark_ready(cpu: CpuId) -> Result<(), SmpInitError> {
    RECORDS
        .get()
        .ok_or(SmpInitError::InvalidTopology)?
        .mark_ready(cpu)
}

/// 置某逻辑 CPU Online（AP：只允许 `Ready → Online`，fail-closed）。
fn set_online(cpu: CpuId) -> Result<(), SmpInitError> {
    RECORDS
        .get()
        .ok_or(SmpInitError::InvalidTopology)?
        .mark_online(cpu)
}

/// AP 在启动屏障上自旋等待放行（调用方保证中断关闭）。
fn wait_for_release(_cpu: CpuId) -> Result<(), SmpInitError> {
    GATE.wait();
    Ok(())
}

/// BSP 等所有已请求的 AP 到达 Ready（或失败）后放行屏障，并打开 BSP 自己的 IPI 源。
///
/// fail-closed：任一 AP `Failed` 或超时即返回错误，绝不发明“部分成功”。
pub fn release_secondaries() -> Result<(), SmpInitError> {
    let registry = RECORDS.get().ok_or(SmpInitError::InvalidTopology)?;
    let bsp = registry.bsp();
    let deadline = boot_deadline();

    for (cpu, record) in registry.records.iter() {
        if cpu == bsp {
            continue;
        }
        loop {
            match record.boot_state() {
                CpuBootState::Starting => {
                    if <arch::TimerImpl as arch::Timer>::now() >= deadline {
                        return Err(SmpInitError::Timeout { cpu });
                    }
                    core::hint::spin_loop();
                }
                // Ready/Online = 已就绪；Offline = 未请求（单 CPU / 未启动的 AP）。
                CpuBootState::Ready | CpuBootState::Online | CpuBootState::Offline => break,
                CpuBootState::Failed => {
                    return Err(SmpInitError::StartRejected {
                        cpu,
                        reason: arch::smp::CpuStartError::Rejected,
                    });
                }
            }
        }
    }

    GATE.release();
    // BSP 现在可接收远端 Reschedule：打开自己的 IPI 源（全局 SIE 由 boot 负责）。
    // UP（只有 BSP）没有 IPI 接收者，也没有注册 handler，故不打开。
    if registry.records.len() > 1 {
        <Backend as Smp>::enable_ipi_interrupt();
    }
    Ok(())
}

/// BSP 等待一组 CPU 到达 Online。
pub fn wait_until_online(targets: &CpuMask, deadline: u64) -> Result<(), SmpInitError> {
    loop {
        let Some(cpu) = targets
            .iter()
            .find(|cpu| cpu_state(*cpu) != Ok(CpuBootState::Online))
        else {
            return Ok(());
        };
        if <arch::TimerImpl as arch::Timer>::now() >= deadline {
            return Err(SmpInitError::Timeout { cpu });
        }
        core::hint::spin_loop();
    }
}

/// AP bring-up / online 的超时时刻（timebase 未知时用兜底窗口）。
fn boot_deadline() -> u64 {
    let hz = TIMEBASE_HZ.load(Ordering::Acquire) as u64;
    let window = if hz == 0 {
        AP_BOOT_TIMEOUT_FALLBACK
    } else {
        hz.saturating_mul(2)
    };
    <arch::TimerImpl as arch::Timer>::now().saturating_add(window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{CpuInfo, DeviceDescriptor, MAX_CPUS, MemoryRegion};

    /// 测试串行化：全局 `RECORDS` 是进程级 `Once`，触碰它的用例必须串行。
    static SMP_TEST_LOCK: crate::test_support::TestLock =
        crate::test_support::TestLock::new(crate::test_support::Rank::Sched);

    fn machine(cpus: &[(bool, u64)]) -> MachineInfo {
        let mut cpu_info = [CpuInfo {
            boot_cpu: false,
            hardware_id: HardwareCpuId::from_raw(0),
        }; MAX_CPUS];
        let mut boot_hardware_id = HardwareCpuId::from_raw(0);
        for (slot, (boot, hardware)) in cpu_info.iter_mut().zip(cpus) {
            *slot = CpuInfo {
                boot_cpu: *boot,
                hardware_id: HardwareCpuId::from_raw(*hardware),
            };
            if *boot {
                boot_hardware_id = HardwareCpuId::from_raw(*hardware);
            }
        }
        MachineInfo {
            boot_hardware_id,
            timebase_frequency: 0,
            cpu_count: cpus.len(),
            cpu_info,
            mem_count: 0,
            memory_regions: [MemoryRegion { base: 0, size: 0 }; 16],
            dev_count: 0,
            devices: [DeviceDescriptor::empty(); 26],
        }
    }

    #[test]
    fn build_rejects_duplicate_hardware_ids() {
        let info = machine(&[(true, 0), (false, 0)]);
        assert_eq!(
            CpuRegistry::build(&info).err(),
            Some(SmpInitError::InvalidTopology)
        );
    }

    #[test]
    fn build_rejects_missing_boot_cpu() {
        // 没有任何 `boot_cpu` 标记，且 `boot_hardware_id` 不匹配任何已发现 CPU。
        let mut info = machine(&[(false, 0), (false, 1)]);
        info.boot_hardware_id = HardwareCpuId::from_raw(99);
        assert_eq!(
            CpuRegistry::build(&info).err(),
            Some(SmpInitError::InvalidTopology)
        );
    }

    #[test]
    fn build_rejects_zero_cpus() {
        let info = machine(&[]);
        assert_eq!(
            CpuRegistry::build(&info).err(),
            Some(SmpInitError::InvalidTopology)
        );
    }

    #[test]
    fn bsp_is_logical_zero_and_others_start_offline() {
        let info = machine(&[(true, 0), (false, 1)]);
        let registry = CpuRegistry::build(&info).unwrap();
        assert_eq!(registry.bsp(), CpuId::from_raw(0));
        assert_eq!(
            registry.state(CpuId::from_raw(0)),
            Ok(CpuBootState::Offline)
        );
        assert_eq!(
            registry.state(CpuId::from_raw(1)),
            Ok(CpuBootState::Offline)
        );
        assert_eq!(
            registry.state(CpuId::from_raw(2)),
            Err(CpuIndexError::OutOfRange)
        );
        assert_eq!(
            registry.hardware_id(CpuId::from_raw(1)),
            Some(HardwareCpuId::from_raw(1))
        );
    }

    /// BSP 不在逻辑 CPU0（boot 未归一化）即拒绝——入口记录 / PLIC 路由依赖该不变式。
    #[test]
    fn build_rejects_boot_hart_not_at_logical_zero() {
        let info = machine(&[(false, 0), (true, 1)]);
        assert_eq!(
            CpuRegistry::build(&info).err(),
            Some(SmpInitError::InvalidTopology)
        );
    }

    #[test]
    fn boot_state_machine_is_monotonic_and_failed_is_terminal() {
        let info = machine(&[(true, 0), (false, 1)]);
        let registry = CpuRegistry::build(&info).unwrap();
        let ap = CpuId::from_raw(1);

        // Offline → Starting（幂等）。
        assert_eq!(registry.request_start(ap), Ok(()));
        assert_eq!(registry.state(ap), Ok(CpuBootState::Starting));
        assert_eq!(registry.request_start(ap), Ok(()));

        // Starting → Ready（幂等）。
        assert_eq!(registry.mark_ready(ap), Ok(()));
        assert_eq!(registry.state(ap), Ok(CpuBootState::Ready));
        assert_eq!(registry.mark_ready(ap), Ok(()));

        // Ready → Online。
        assert_eq!(registry.mark_online(ap), Ok(()));
        assert_eq!(registry.state(ap), Ok(CpuBootState::Online));
        assert_eq!(registry.mark_ready(ap), Ok(()));

        // Failed 是终态：不得被 request_start / mark_ready 复活。
        registry.mark_failed(ap);
        assert_eq!(registry.state(ap), Ok(CpuBootState::Failed));
        assert_eq!(
            registry.request_start(ap),
            Err(SmpInitError::InvalidTopology)
        );
        assert_eq!(registry.mark_ready(ap), Err(SmpInitError::InvalidTopology));
    }

    #[test]
    fn mark_ready_requires_request_start_first() {
        let info = machine(&[(true, 0), (false, 1)]);
        let registry = CpuRegistry::build(&info).unwrap();
        assert_eq!(
            registry.mark_ready(CpuId::from_raw(1)),
            Err(SmpInitError::InvalidTopology),
            "Offline AP cannot become Ready without request_start"
        );
    }

    #[test]
    fn online_scan_reflects_only_online_records() {
        let info = machine(&[(true, 0), (false, 1), (false, 2)]);
        let registry = CpuRegistry::build(&info).unwrap();
        registry.publish_bsp_online(CpuId::from_raw(0)).unwrap();
        registry.request_start(CpuId::from_raw(1)).unwrap();
        registry.mark_ready(CpuId::from_raw(1)).unwrap();
        registry.mark_online(CpuId::from_raw(1)).unwrap();

        let mask = registry.online();
        assert!(mask.contains(CpuId::from_raw(0)));
        assert!(mask.contains(CpuId::from_raw(1)));
        assert!(!mask.contains(CpuId::from_raw(2)));
        assert_eq!(mask.count(), 2);
    }

    /// 保证全局记录表已发布（`init` 幂等；测试间执行顺序不保证，故共享同一拓扑）。
    fn ensure_global_records() {
        if RECORDS.get().is_none() {
            let _ = init(&machine(&[(true, 0), (false, 1)]));
        }
    }

    /// 全局 `init`：发布记录、注册 Core IPI 回调（host Fake 只记录）、BSP Online、
    /// 其余 Offline。进程级 `Once`，测试共享同一份记录（同锁串行）。
    #[test]
    fn global_init_publishes_records_and_marks_bsp_online() {
        let _serial = SMP_TEST_LOCK.lock();
        ensure_global_records();

        assert_eq!(cpu_state(CpuId::from_raw(0)), Ok(CpuBootState::Online));
        assert_eq!(cpu_state(CpuId::from_raw(1)), Ok(CpuBootState::Offline));
        assert_eq!(online_cpus().count(), 1);
        assert!(online_cpus().contains(CpuId::from_raw(0)));
        assert_eq!(
            hardware_id(CpuId::from_raw(1)),
            Some(HardwareCpuId::from_raw(1))
        );

        // Core IPI 回调已注册（host Fake 可观察）。
        assert!(
            arch::fake::registered_ipi_handler_for_test().is_some(),
            "smp::init must register the Core IPI handler for multi-CPU topologies"
        );
    }

    /// pending work 位是每 CPU 独立的：操作一个 CPU 不影响另一个。
    #[test]
    fn pending_ipi_bits_are_per_cpu() {
        let _serial = SMP_TEST_LOCK.lock();
        ensure_global_records();
        let a = CpuId::from_raw(0);
        let b = CpuId::from_raw(1);
        let _ = ipi::take_pending(a);
        let _ = ipi::take_pending(b);
        ipi::ipi_interrupt(a);
        assert_eq!(ipi::take_pending(b), 0);
        assert_eq!(ipi::take_pending(a), ipi::RESCHEDULE_BIT);
    }

    /// `notify` 先发布 pending 位、再响铃；host Fake 记录发出的硬件 id。
    #[test]
    fn notify_publishes_pending_then_rings_doorbell() {
        let _serial = SMP_TEST_LOCK.lock();
        ensure_global_records();
        let ap = CpuId::from_raw(1);
        let _ = ipi::take_pending(ap);
        let _ = arch::fake::take_sent_ipis_for_test();

        let mut targets = CpuMask::empty();
        targets.insert(ap).unwrap();
        assert_eq!(ipi::notify(&targets, IpiRequest::Reschedule), Ok(()));

        assert_eq!(ipi::take_pending(ap), ipi::RESCHEDULE_BIT);
        assert_eq!(arch::fake::take_sent_ipis_for_test(), alloc::vec![1]);
    }

    /// `notify` 对没有记录的逻辑 CPU 返回 `InvalidTarget`，且不响铃。
    #[test]
    fn notify_rejects_targets_without_records() {
        let _serial = SMP_TEST_LOCK.lock();
        ensure_global_records();
        let _ = arch::fake::take_sent_ipis_for_test();

        let mut targets = CpuMask::empty();
        targets.insert(CpuId::from_raw(5)).unwrap(); // 容量内、但超过 cpu_count(2)
        assert_eq!(
            ipi::notify(&targets, IpiRequest::Reschedule),
            Err(NotifyError::InvalidTarget)
        );
        assert!(arch::fake::take_sent_ipis_for_test().is_empty());
    }

    /// `drain_pending` 把 pending 的 Reschedule 转成「需要重调度」标志，且只置一次。
    #[test]
    fn drain_pending_turns_reschedule_into_flag() {
        let _serial = SMP_TEST_LOCK.lock();
        ensure_global_records();
        let ap = CpuId::from_raw(1);
        let _ = ipi::take_pending(ap);
        let _ = take_resched(ap);

        ipi::ipi_interrupt(ap);
        ipi::drain_pending(ap);

        assert_eq!(ipi::take_pending(ap), 0, "drain consumes the pending bits");
        assert!(
            take_resched(ap),
            "Reschedule became a deferred-reschedule flag"
        );
        assert!(!take_resched(ap), "the flag is consumed once");
    }
}
