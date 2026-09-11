//! Handle / Authority：类型化、不可伪造的授权。
//!
//! 通用 token/slot 机制 + `MmioHandle` / `IrqHandle`。Handle 的 authority
//! 不来自 token bits（`to_raw` 可被伪造），而来自 Core 对 slot 存在性、
//! generation、owner 和资源生命周期的验证。
//!
//! C6 起步：`mmio` 有 `claim`（设备认领：request → Core authorize → grant）
//! 与 `read_u32`（每次访问重新验证 handle 后由 Core 访问硬件）；各表的全局
//! 初始化由 [`init`] 汇总。不发明 `DeviceHandle`：设备 owner 直接记录在
//! MMIO 表 slot 上（将来 IRQ 从同一 owner 派生）。
//!
//! 暂不实现 DmaHandle、TaskHandle、TimerHandle 或其他资源管理器；也不在这里
//! 建立 ResourceDomain 集合。各资源表自己的 owner 记录共同构成组件的资源视图。

mod error;
mod generic;
pub mod irq;
pub mod mmio;
mod table;

/// 初始化所有资源 authority 表（`core::init` 调用一次）。
///
/// 各资源表的全局初始化在这里汇总；IRQ / DMA 表的全局化接入时在此追加。
pub fn init() {
    mmio::init();
    // TODO(C6): irq::init(); —— IrqTable 全局化后接入
}

pub use error::HandleError;
pub use generic::Handle;
pub use irq::IrqHandle;
pub use mmio::MmioHandle;

pub(crate) use generic::Slot;
pub(crate) use table::ResourceTable;
