//! 架构中立的 **CPU 身份** 类型与本地中断回调形状。
//!
//! # 两类 CPU 身份，严格分离
//!
//! - [`CpuId`]：**逻辑**、稠密、从 0 连续编号；Core 用它索引 per-CPU 状态、
//!   记 `Running(CpuId)`、做 `CpuMask`。由 Core 从 discovery 赋号，arch 只存。
//! - [`HardwareCpuId`]：**硬件**身份（RISC-V hartid、x86 APIC ID、AArch64 MPIDR
//!   affinity、LoongArch CPUID）。可能稀疏、可能非零起点，编解码随 ISA 而变。
//!
//! 硬件身份**不是**数组下标：绝不 `CpuId(hart_id)`。Core 校验 discovery 后建立
//! 硬件 ↔ 逻辑映射；arch 的“指定某个 CPU”的机制（`Smp::start_cpu` / `send_ipi`）
//! 用 [`HardwareCpuId`]，而回调 / per-CPU 绑定用 [`CpuId`]。
//!
//! `CpuId` 定义在 arch、由 Core `re-export`（`core::machine::CpuId`），因此 arch
//! 的 trait 签名能用逻辑 id，而 arch 不依赖 Core。

/// 逻辑 CPU 身份：稠密、从 0 连续，由 Core 从 discovery 赋号。
///
/// 用作 per-CPU 数组下标；**不是**硬件 hart id（见 [`HardwareCpuId`]）。
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CpuId(pub usize);

impl core::fmt::Display for CpuId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "CPU{}", self.0)
    }
}

impl CpuId {
    /// 构造（调用方保证它来自 Core 的连续赋号，而不是硬件 id 强转）。
    pub const fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    /// 原始逻辑下标。
    pub const fn raw(self) -> usize {
        self.0
    }
}

/// 硬件 CPU 身份（hartid / APIC ID / MPIDR affinity / CPUID）。
///
/// **不是数组下标。** 稀疏、可非零起点，编解码由各 ISA 后端负责。
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HardwareCpuId(pub u64);

impl HardwareCpuId {
    /// 原始硬件身份值。
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// 由原始硬件身份值构造。
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

impl core::fmt::Display for HardwareCpuId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "hwcpu{}", self.0)
    }
}

/// 本 CPU 局部中断（timer / IPI）回调的**唯一注册形状**。
///
/// arch 在当前执行 CPU 上调用它，并传入**逻辑** `CpuId`（UP 阶段为 `CpuId(0)`；
/// SMP 阶段从 CPU-local 入口记录读取已绑定的逻辑 id）。Core 据此知道是谁触发的。
///
/// 这是内核内部（同一链接镜像）的回调，不是组件 C ABI；用 Rust `fn` 指针即可。
/// `Timer` / `Smp` 的注册方法统一使用本类型；外部中断用 [`ExternalIrqHandler`]。
pub type LocalInterruptHandler = fn(CpuId);

/// 外部中断回调形状：`(逻辑 CpuId, 逻辑 IRQ 号)`。
///
/// 外部中断控制器后端拥有 **ack/EOI 与源映射**：claim / ack 令牌、向量 / INTID /
/// source 号 → 逻辑 IRQ 号的映射、spurious-interrupt 规则全部 backend-private。
/// 后端在自己的分发里**每条中断调用一次**本回调，回调返回后才 complete/EOI；
/// Core 只按逻辑 IRQ 号查 route 表，看不到令牌、向量或 INTID。
///
/// 与 [`LocalInterruptHandler`] 同一性质：内核内部（同一链接镜像）的回调，
/// 不是组件 C ABI；用 Rust `fn` 指针即可。
pub type ExternalIrqHandler = fn(CpuId, u32);
