//! SMP 的 **arch 契约**：CPU 启动与 IPI 传输（机制，不含策略）。
//!
//! # 边界
//!
//! 这个 trait 只做两件事：
//!
//! 1. **启动一个次 CPU**（`start_cpu`）：arch 负责到达目标 CPU 的硬件/固件
//!    握手与入口环境（地址空间、栈、ABI、关中断），Core 负责“是否/何时/给谁”
//!    以及启动后的逻辑身份与本地子系统初始化。
//! 2. **传一个门铃**（`send_ipi` / `send_ipi_mask`）：arch **不解释** IPI 含义，
//!    不知道 `Reschedule`、组件 id、TLB shootdown 代数；Core 先发布 pending work，
//!    再让 arch 响铃；arch 只在回调前后处理硬件 ack/EOI。
//!
//! # 明确不做
//!
//! - **不做**运行队列、负载均衡、调度算法（策略在 Component / Core，不在 arch）。
//! - **不做**通用跨 CPU RPC、任意远端闭包。
//! - **不做**远端 timer 编程（本地 timer 本地编程；远端语义属于 Core 的发布+通知机制）。
//! - **不暴露**通用 `ack_ipi()`：SBI 软件中断、APIC vector、GIC SGI、LoongArch
//!   IOCSR IPI 的 ack/complete 序列各不相同，通用化只会丢信息。
//! - **不承诺** `start_cpu` 返回 = CPU 已 online。`Ok` 只表示“启动请求被接受”。
//!
//! # 与 Timer / InterruptController 的关系
//!
//! 三者的**回调注册签名已经统一**为 [`LocalInterruptHandler`]（`fn(CpuId)`）：
//! trap 分发在 Rust 侧把逻辑 CPU 身份传给回调，**不需要改 trap 汇编**。
//!
//! 仍待各自补齐的是 `init_ipi_cpu()`（只初始化**当前执行 CPU** 的本地 IPI
//! 接收，且保持 masked），以及 `Timer`/`InterruptController` 的原地接口收敛
//! （per-CPU 初始化、claim/complete 配对）。这些属于 SMP 实现工作；本骨架只
//! 固化签名与边界。
//!
//! 逻辑身份、启动状态、pending work、调度归属、启动屏障全部在 **Core**（`os/core/src/smp/`）。

use crate::cpu::HardwareCpuId;

/// 本 CPU 局部中断回调（timer / external / IPI 共用形状）。见 [`crate::cpu::LocalInterruptHandler`]。
pub use crate::cpu::LocalInterruptHandler;

/// 次 CPU 的入口。由 `Smp::start_cpu` 经后端 trampoline 进入。
///
/// `argument` = Core 传下的不透明值（约定为逻辑 CPU 的稠密下标，由 Core 校验）。
/// 返回类型 `!`：AP 一旦进入就不再返回。
pub type SecondaryEntry = unsafe extern "C" fn(argument: usize) -> !;

/// 次 CPU 的启动描述符。**由 Core 构造**，arch 只消费。
#[derive(Clone, Copy)]
pub struct SecondaryBoot {
    /// AP 初始栈的 Core 虚拟地址区间（arch 按自己的入口环境映射/使用）。
    pub stack_base: usize,
    /// 栈字节数。
    pub stack_bytes: usize,
    /// 建立后端 CPU 环境后跳入的 Rust 入口。
    pub entry: SecondaryEntry,
    /// 传给 `entry` 的不透明值。
    pub argument: usize,
}

/// `Smp::prepare` / `init_cpu` / `register_ipi_handler` 失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitError {
    /// 该 ISA / 平台不支持 SMP。
    Unsupported,
    /// 拓扑 / 配置不合法（越界、缺 discovery、地址不可映射等）。
    InvalidConfiguration,
    /// 已经初始化过（`prepare` / `register_ipi_handler` 只允许一次）。
    AlreadyInitialized,
    /// 固件 / 硬件初始化失败。
    HardwareFailure,
}

/// `Smp::start_cpu` 失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuStartError {
    /// 该 ISA / 平台不支持启动次 CPU。
    Unsupported,
    /// 目标硬件身份不在有效集合内。
    InvalidTarget,
    /// 该 CPU 已在启动或已启动。
    AlreadyStarted,
    /// 固件 / 硬件拒绝了启动请求。
    Rejected,
    /// 无法判定是否已启动（AP 仍可能在之后进入）。必须保留描述符与栈。
    Indeterminate,
}

/// `Smp::send_ipi*` 失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpiError {
    /// 该 ISA / 平台不支持 IPI。
    Unsupported,
    /// 目标硬件身份无效。
    InvalidTarget,
    /// 投递失败。注意：`send_ipi_mask` 可能**部分投递**后才返回错误，
    /// Core 必须保留 pending work，不得把失败理解为“什么都没送到”。
    DeliveryFailed,
}

/// 每架构 SMP backend：CPU 启动 + IPI 传输。
///
/// 具体实现见各 ISA 子模块（`os/arch/src/<isa>/smp.rs`）；Core 只经
/// `SmpImpl` 别名与本 trait 依赖。**约定：本节方法体在骨架阶段为 `todo!()`。**
pub trait Smp {
    /// 后端特有的启动配置，**由 boot 填充**，不是通用 Core 能构造的东西。
    /// 例如：RISC-V 的 SBI/直接启动资源与物理 trampoline；x86 的低内存
    /// trampoline 与 bootstrap 页表；AArch64 的 PSCI conduit；LoongArch 的
    /// mailbox 启动信息。
    type BootConfig;

    /// BSP 在启动任何次 CPU 之前调用一次。
    ///
    /// # Safety
    /// `config` 引用的映射、trampoline 内存、固件资源必须满足后端要求的生存期。
    unsafe fn prepare(config: &'static Self::BootConfig) -> Result<(), InitError>;

    /// 请求启动次 CPU。**只表示请求被接受，不表示 CPU 已 online。**
    ///
    /// 后端经自己的汇编 trampoline 进入，负责建立：内核地址空间（若适用）、
    /// 提供的栈与 ABI 对齐、指令可见性与内存发布、`tp = 0`、本地中断关闭、
    /// 以及调用 Rust 入口前的架构前提。Core 入口随后安装逻辑身份与本地存储。
    ///
    /// # Safety
    /// boot 与它的栈必须在任何可能的迟到入口时仍驻留；`boot.entry` / `argument`
    /// 必须满足入口函数的契约。
    unsafe fn start_cpu(
        target: HardwareCpuId,
        boot: &'static SecondaryBoot,
    ) -> Result<(), CpuStartError>;

    /// 初始化**当前执行 CPU** 的 IPI 接收机制（仍保持 masked）。
    ///
    /// 这是 IPI 接收的本地初始化，**不是** [`crate::CpuArch::init_cpu`]（trap
    /// 状态）或 [`crate::Timer::init_cpu`]（本地 timer）；BSP 与每个 AP 都要在
    /// 本地调用一次。
    fn init_ipi_cpu() -> Result<(), InitError>;

    /// 全局注册一次 IPI 回调，必须在任何 CPU 打开 IPI 接收之前完成。
    fn register_ipi_handler(handler: LocalInterruptHandler) -> Result<(), InitError>;

    /// 只解除**当前执行 CPU** 的 IPI 源屏蔽（不动全局中断使能位）。
    fn enable_ipi_interrupt();

    /// 发一个**可合并的**门铃（不是计数消息）。目标用硬件身份。
    fn send_ipi(target: HardwareCpuId) -> Result<(), IpiError>;

    /// 给一组硬件身份发门铃。可能部分投递后才返回错误。
    fn send_ipi_mask(targets: &[HardwareCpuId]) -> Result<(), IpiError>;
}
