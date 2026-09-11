//! Core ABI 错误码命名空间（Errno）：内部错误 → 跨边界错误码的唯一翻译点。
//!
//! # ABI 错误约定（v3 起）
//!
//! ```text
//! 0          success
//! -negative  failure: -Errno
//! ```
//!
//! - **内部保持丰富**：`TaskError` / `HandleError` / `ComponentLoadError` /
//!   `SchedError` / `InterfaceError` / `MmioError` 等继续各自为政（强类型、
//!   可重构），只在 Core ABI 边界翻译成 `Errno`——本文件是唯一映射表。
//! - **数值稳定**：Linux/POSIX 风格；进入 public ABI 后，数字不再变更。
//! - 组件（Rust / C / Wasm / IPC）只需要理解这一套错误码。
//! - 有返回值的 action 统一 `i32 status + out 参数`（`0` / `-errno`），
//!   不再新增"正数成功 / 负数错误"协议；纯 query 与 allocator 风格 API
//!   不强制（见 `component/export.rs`）。

use crate::component::interface::InterfaceError;
use crate::component::load::ComponentLoadError;
use crate::handle::{HandleError, irq, mmio};
use crate::sched::SchedError;
use crate::task::TaskError;

/// 稳定、Linux/POSIX 风格的 ABI 错误码。用到哪个加哪个；数字不再改。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Errno {
    EPERM = 1,
    ENOENT = 2,
    ESRCH = 3,
    EIO = 5,
    ENOEXEC = 8,
    EBADF = 9,
    EAGAIN = 11,
    ENOMEM = 12,
    EACCES = 13,
    EFAULT = 14,
    EBUSY = 16,
    EEXIST = 17,
    ENODEV = 19,
    EINVAL = 22,
    ENOSYS = 38,
    EOVERFLOW = 75,
    ENOTSUP = 95,
    EALREADY = 114,
    ESTALE = 116,
    EKEYREVOKED = 128,
}

impl Errno {
    /// syscall 风格返回值：`0` 或 `-errno`。
    pub const fn code(self) -> i32 {
        -(self as i32)
    }
}

/// `Result<(), E>` → ABI status（仅 action；值型 API 用 `status + out`）。
pub(crate) fn status<E: Into<Errno>>(result: Result<(), E>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => error.into().code(),
    }
}

impl From<HandleError> for Errno {
    fn from(error: HandleError) -> Self {
        match error {
            HandleError::Invalid => Errno::EBADF,
            HandleError::Stale => Errno::ESTALE,
            HandleError::WrongOwner => Errno::EACCES,
            HandleError::Revoked => Errno::EKEYREVOKED,
            HandleError::AlreadyReleased => Errno::EALREADY,
        }
    }
}

impl From<TaskError> for Errno {
    fn from(error: TaskError) -> Self {
        match error {
            TaskError::AlreadyExists => Errno::EEXIST,
            TaskError::NotFound => Errno::ESRCH,
            TaskError::NoMemory => Errno::ENOMEM,
            TaskError::InvalidTransition => Errno::EINVAL,
            TaskError::RequesterNotFound => Errno::ESRCH,
            TaskError::RequesterNotReady => Errno::EAGAIN,
            TaskError::WrongOwner => Errno::EACCES,
            TaskError::EntryOutOfImage => Errno::EFAULT,
        }
    }
}

impl From<SchedError> for Errno {
    fn from(error: SchedError) -> Self {
        match error {
            SchedError::NoPolicy => Errno::ENOTSUP,
            SchedError::InvalidTransition => Errno::EINVAL,
            SchedError::NotFound => Errno::ESRCH,
            SchedError::NoCurrent => Errno::ESRCH,
        }
    }
}

impl From<InterfaceError> for Errno {
    fn from(error: InterfaceError) -> Self {
        match error {
            InterfaceError::ProviderNotFound => Errno::ESRCH,
            InterfaceError::ProviderNotReady => Errno::EAGAIN,
            InterfaceError::UnknownInterface => Errno::ENOENT,
            InterfaceError::KindMismatch => Errno::EINVAL,
            InterfaceError::VersionMismatch => Errno::EINVAL,
            InterfaceError::Unbound => Errno::ENOENT,
            InterfaceError::BindingNotFound => Errno::ENOENT,
            InterfaceError::IdExhausted => Errno::EOVERFLOW,
        }
    }
}

impl From<ComponentLoadError> for Errno {
    fn from(error: ComponentLoadError) -> Self {
        match error {
            ComponentLoadError::StoreNotMounted => Errno::ENODEV,
            ComponentLoadError::NotFound => Errno::ENOENT,
            ComponentLoadError::ReadFailed => Errno::EIO,
            ComponentLoadError::Loader(_) => Errno::ENOEXEC,
            ComponentLoadError::DeclareFailed => Errno::EEXIST,
            ComponentLoadError::ResolveFailed => Errno::ENOENT,
            ComponentLoadError::StartFailed => Errno::EIO,
            ComponentLoadError::InitFailed(_) => Errno::EIO,
        }
    }
}

impl From<mmio::MmioClaimError> for Errno {
    fn from(error: mmio::MmioClaimError) -> Self {
        match error {
            mmio::MmioClaimError::DeviceNotFound => Errno::ENODEV,
            mmio::MmioClaimError::DeviceBusy => Errno::EBUSY,
            mmio::MmioClaimError::Denied => Errno::EPERM,
        }
    }
}

impl From<mmio::MmioError> for Errno {
    fn from(error: mmio::MmioError) -> Self {
        match error {
            mmio::MmioError::Handle(inner) => inner.into(),
            mmio::MmioError::OutOfBounds | mmio::MmioError::Unaligned => Errno::EINVAL,
        }
    }
}

impl From<irq::IrqClaimError> for Errno {
    fn from(error: irq::IrqClaimError) -> Self {
        match error {
            irq::IrqClaimError::DeviceNotFound => Errno::ENODEV,
            irq::IrqClaimError::DeviceHasNoIrq => Errno::ENODEV,
            irq::IrqClaimError::LineBusy => Errno::EBUSY,
            irq::IrqClaimError::Denied => Errno::EPERM,
        }
    }
}

impl From<irq::IrqError> for Errno {
    fn from(error: irq::IrqError) -> Self {
        match error {
            irq::IrqError::Handle(inner) => inner.into(),
            irq::IrqError::NoDelivery => Errno::EINVAL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_negative_errno() {
        assert_eq!(Errno::EPERM.code(), -1);
        assert_eq!(Errno::ENOENT.code(), -2);
        assert_eq!(Errno::EINVAL.code(), -22);
        assert_eq!(Errno::ENOTSUP.code(), -95);
        assert_eq!(Errno::ESTALE.code(), -116);
        assert_eq!(Errno::EKEYREVOKED.code(), -128);
    }

    #[test]
    fn internal_errors_translate_at_the_boundary() {
        assert_eq!(Errno::from(HandleError::Stale), Errno::ESTALE);
        assert_eq!(Errno::from(HandleError::WrongOwner), Errno::EACCES);
        assert_eq!(Errno::from(TaskError::NoMemory), Errno::ENOMEM);
        assert_eq!(Errno::from(TaskError::InvalidTransition), Errno::EINVAL);
        assert_eq!(Errno::from(SchedError::NoPolicy), Errno::ENOTSUP);
        assert_eq!(Errno::from(InterfaceError::ProviderNotReady), Errno::EAGAIN);
        assert_eq!(Errno::from(ComponentLoadError::NotFound), Errno::ENOENT);
        assert_eq!(Errno::from(mmio::MmioClaimError::DeviceBusy), Errno::EBUSY);
        assert_eq!(
            Errno::from(mmio::MmioError::Handle(HandleError::Revoked)),
            Errno::EKEYREVOKED
        );
        assert_eq!(Errno::from(mmio::MmioError::Unaligned), Errno::EINVAL);
        assert_eq!(Errno::from(irq::IrqClaimError::LineBusy), Errno::EBUSY);
        assert_eq!(
            Errno::from(irq::IrqClaimError::DeviceHasNoIrq),
            Errno::ENODEV
        );
        assert_eq!(
            Errno::from(irq::IrqError::Handle(HandleError::Stale)),
            Errno::ESTALE
        );
        assert_eq!(Errno::from(irq::IrqError::NoDelivery), Errno::EINVAL);
    }

    #[test]
    fn status_maps_ok_and_err() {
        assert_eq!(status(Ok::<(), HandleError>(())), 0);
        assert_eq!(status(Err::<(), HandleError>(HandleError::Invalid)), -9);
    }
}
