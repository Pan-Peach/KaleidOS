//! Handle / Authority：类型化的 Core authority token（token 可被伪造，authority 由 Core 验证）。
//!
//! 通用 token/slot 机制 + `MmioHandle` / `IrqHandle`。Handle 的 authority
//! 不来自 token bits（`to_raw` 可被伪造），而来自 Core 对 slot 存在性、
//! generation、owner 和资源生命周期的验证。
//!
//! C6：`mmio` 有 `claim`（设备认领：request → Core authorize → grant）、
//! `read_u32` / `write_u32`（每次访问重新验证 handle 后由 Core 访问硬件）、
//! `release`，以及 `derive_lease`（一次性校验后派生 `(ptr,len)` 供受信
//! KernelNative 直访，撤销为协作式）；`irq` 支持回调与轮询两种投递；
//! `dma` 有 `alloc`（设备身份从 caller 已持有的 `MmioHandle` 推导，绝不接受
//! 自报设备号）、`derive_lease`（backing ptr/len + 设备可见地址 + provenance）、
//! `release` / `revoke_owner`（backing lease 进 Core 私有 QUARANTINE，不 free）。
//! 各表的全局初始化由 [`init`] 汇总。不发明 `DeviceHandle`：设备 owner 直接
//! 记录在 MMIO 表 slot 上（IRQ / DMA 的设备身份都从同一 authority 派生）。
//!
//! 暂不实现 TaskHandle、TimerHandle 或其他资源管理器；也不在这里建立
//! ResourceDomain 集合。各资源表自己的 owner 记录共同构成组件的资源视图。

mod context;
pub mod dma;
mod error;
mod generic;
pub mod irq;
mod lease;
pub mod mmio;
mod table;

/// 初始化所有资源 authority 表（`core::init` 调用一次）。
///
/// 各资源表的全局初始化在这里汇总（MMIO / IRQ / DMA）。
/// 外部中断的**投递回调**注册不在这里——那是 `crate::irq::init` 的职责。
pub fn init() {
    mmio::init();
    irq::init();
    dma::init();
}

pub use context::RequestContext;
pub use dma::DmaHandle;
pub use error::HandleError;
pub use generic::Handle;
pub use irq::IrqHandle;
pub use lease::{DmaView, MmioView};
pub use mmio::MmioHandle;

pub(crate) use generic::Slot;
pub(crate) use table::ResourceTable;

/// 资源种类：authority 事件的类型化标签（trace / 记账用）。
///
/// 只描述"哪一类 authority"，不携带地址、设备号或 IRQ 线 —— 那些是各资源表
/// 内部的真相。与具体 Handle 类型一一对应（`Mmio`→`MmioHandle`，…）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Mmio,
    Irq,
    Dma,
}

/// 跨 ABI 的 opaque handle 编码（见 `Handle::to_raw`：`slot << 32 | generation`）。
///
/// 只保证"可传递、可比较、可记录"；它**不是** authority —— 数字可被伪造，
/// 真正的权限永远来自资源表对 slot/generation/owner/生命周期的验证。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RawHandle(u64);

impl RawHandle {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}
