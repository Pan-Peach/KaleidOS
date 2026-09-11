//! Handle / Authority：类型化、不可伪造的授权骨架。
//!
//! 第一阶段只搭通用 token/slot 结构，以及 C6 需要的 `MmioHandle` /
//! `IrqHandle`。Handle 的 authority 不来自 token bits，而来自 Core 对 slot
//! 存在性、generation、owner 和资源生命周期的验证。
//!
//! 暂不实现 DmaHandle、TaskHandle、TimerHandle 或其他资源管理器；也不在这里
//! 建立 ResourceDomain 集合。各资源表自己的 owner 记录共同构成组件的资源视图。

mod error;
mod generic;
mod table;
pub mod irq;
pub mod mmio;

pub use error::HandleError;
pub use generic::Handle;
pub use irq::IrqHandle;
pub use mmio::MmioHandle;

pub(crate) use generic::Slot;
pub(crate) use table::ResourceTable;
