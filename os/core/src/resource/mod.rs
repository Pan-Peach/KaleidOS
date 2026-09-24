//! Core resource bookkeeping：device ownership / IRQ routes / DMA mappings。
//!
//! 这里**不是** capability 系统。Core 只维护裸机程序无法自己知道的那部分真相：
//!
//! - **device ownership**：哪台设备被哪个 Component 认领（用于独占、unload、
//!   失败清理与复用）。不是"每次访问重新鉴权"。
//! - **IRQ routes**：哪条中断线归哪个 owner、回调是谁（trap 需要一个锚点）。
//! - **DMA mappings**：哪个设备能看到哪段 buffer（No-IOMMU 时就是 identity，
//!   但 seam 为 IOMMU / bounce buffer 保留）。
//!
//! # 访问强制（access enforcement）不在 KernelNative 数据路径上
//!
//! KernelNative 与 Core 同特权、共享内核地址空间：组件本来就能访问裸地址。
//! 通过 `Handle -> validate -> Core MMIO read/write` 来"保护" MMIO 没有真实
//! 安全意义。受信 KernelNative 驱动 claim 后直接拿到 MMIO 指针，
//! steady state 不再进 Core。
//!
//! 真正的访问强制来自执行域（mechanism，不是每次 API 鉴权）：
//!
//! ```text
//! KernelNative   trusted / raw pointer / 不做硬件访问限制
//! Isolated       private address space / claim 后建立 mapping
//!                未映射访问 → fault（由页表强制）
//! Sandboxed    privilege + private address space（未实现）
//! ```
//!
//! 当前 [`device::claim`] 直接返回寄存器基址指针；Isolated 分支在同一 seam 里
//! 把窗口映射进组件地址空间再返回 VA。上层 driver 的寄存器访问逻辑不因
//! execution domain 改变而重写。
//!
//! # 保留的 correctness 不变量（不是 security）
//!
//! - 设备独占（一台设备最多一个 owner）——防重复认领 / 复用竞态。
//! - 失败 quarantine：组件失败 ≠ 设备已静默；DMA backing 不得立即 free/复用。
//! - 拆机顺序：仍有 live IRQ route / DMA mapping 时拒绝释放 device。

pub mod context;
pub mod device;
pub mod dma;
pub mod irq;

/// 资源种类：trace / 记账用的类型化标签。
///
/// 只描述"哪一类资源"，不携带地址或身份——那些在各表内部。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Device,
    Irq,
    Dma,
}

/// 初始化资源表（`core::init` 调用一次）。
pub fn init() {
    device::init();
    irq::init();
    dma::init();
}

pub use context::RequestContext;
