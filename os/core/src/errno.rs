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

use crate::component::exit::ComponentStopError;
use crate::component::interface::InterfaceError;
use crate::component::load::ComponentLoadError;
use crate::handle::{HandleError, dma, irq, mmio};
use crate::machine;
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
            InterfaceError::AbiMismatch => Errno::EINVAL,
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
            ComponentLoadError::ImageFailed => Errno::EEXIST,
            ComponentLoadError::DeclareFailed => Errno::EEXIST,
            ComponentLoadError::ResolveFailed => Errno::ENOENT,
            ComponentLoadError::StartFailed => Errno::EIO,
            ComponentLoadError::CreateFailed(_) => Errno::EIO,
            ComponentLoadError::CreatePanicked => Errno::EIO,
            ComponentLoadError::DestroyFailed(_) => Errno::EIO,
            ComponentLoadError::DestroyPanicked => Errno::EIO,
            ComponentLoadError::InterfaceCommitFailed(_) => Errno::EINVAL,
            ComponentLoadError::TaskPanicked(_) => Errno::EIO,
        }
    }
}

impl From<ComponentStopError> for Errno {
    fn from(error: ComponentStopError) -> Self {
        match error {
            ComponentStopError::NotFound => Errno::ENOENT,
            // 状态机拒绝 `Ready → Stopping`（重复 stop / 非 Ready 实例）。
            ComponentStopError::NotReady => Errno::EINVAL,
            // 实例仍被任务占用（Linux `delete_module` 的 EBUSY 类比）。
            ComponentStopError::OwnsLiveTasks => Errno::EBUSY,
            ComponentStopError::DestroyFailed(_) => Errno::EIO,
            ComponentStopError::DestroyPanicked => Errno::EIO,
            ComponentStopError::StateRejected => Errno::EIO,
        }
    }
}

impl From<machine::DeviceLookupError> for Errno {
    fn from(error: machine::DeviceLookupError) -> Self {
        match error {
            machine::DeviceLookupError::NoMachineInfo => Errno::ENODEV,
            machine::DeviceLookupError::NoSuchOrdinal => Errno::ENOENT,
        }
    }
}

impl From<mmio::MmioClaimError> for Errno {
    fn from(error: mmio::MmioClaimError) -> Self {
        match error {
            mmio::MmioClaimError::DeviceNotFound => Errno::ENODEV,
            mmio::MmioClaimError::NotMmio => Errno::ENOTSUP,
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
            mmio::MmioError::HasChildren => Errno::EBUSY,
        }
    }
}

impl From<dma::DmaError> for Errno {
    fn from(error: dma::DmaError) -> Self {
        match error {
            dma::DmaError::Handle(inner) => inner.into(),
            dma::DmaError::Mmio(inner) => inner.into(),
            dma::DmaError::InvalidSize => Errno::EINVAL,
            dma::DmaError::Exhausted => Errno::ENOMEM,
        }
    }
}

impl From<irq::IrqClaimError> for Errno {
    fn from(error: irq::IrqClaimError) -> Self {
        match error {
            irq::IrqClaimError::DeviceNotFound => Errno::ENODEV,
            irq::IrqClaimError::DeviceHasNoIrq => Errno::ENODEV,
            irq::IrqClaimError::LineBusy => Errno::EBUSY,
            irq::IrqClaimError::MmioHandle(inner) => inner.into(),
            irq::IrqClaimError::Denied => Errno::EPERM,
        }
    }
}

impl From<irq::IrqError> for Errno {
    fn from(error: irq::IrqError) -> Self {
        match error {
            irq::IrqError::Handle(inner) => inner.into(),
            irq::IrqError::NoDelivery => Errno::EINVAL,
            irq::IrqError::NotPolled => Errno::EINVAL,
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
        assert_eq!(
            Errno::from(ComponentLoadError::DestroyFailed(1)),
            Errno::EIO
        );
        assert_eq!(Errno::from(ComponentLoadError::DestroyPanicked), Errno::EIO);
        // 优雅停止的错误在边界处有稳定档位（当前消费者 = monitor unload / host tests）。
        assert_eq!(Errno::from(ComponentStopError::NotFound), Errno::ENOENT);
        assert_eq!(Errno::from(ComponentStopError::NotReady), Errno::EINVAL);
        assert_eq!(Errno::from(ComponentStopError::OwnsLiveTasks), Errno::EBUSY);
        assert_eq!(
            Errno::from(ComponentStopError::DestroyFailed(1)),
            Errno::EIO
        );
        assert_eq!(Errno::from(ComponentStopError::DestroyPanicked), Errno::EIO);
        assert_eq!(Errno::from(ComponentStopError::StateRejected), Errno::EIO);
        assert_eq!(Errno::from(mmio::MmioClaimError::DeviceBusy), Errno::EBUSY);
        assert_eq!(Errno::from(mmio::MmioClaimError::NotMmio), Errno::ENOTSUP);
        assert_eq!(
            Errno::from(mmio::MmioError::Handle(HandleError::Revoked)),
            Errno::EKEYREVOKED
        );
        assert_eq!(Errno::from(mmio::MmioError::Unaligned), Errno::EINVAL);
        assert_eq!(Errno::from(mmio::MmioError::HasChildren), Errno::EBUSY);
        assert_eq!(
            Errno::from(machine::DeviceLookupError::NoMachineInfo),
            Errno::ENODEV
        );
        assert_eq!(
            Errno::from(machine::DeviceLookupError::NoSuchOrdinal),
            Errno::ENOENT
        );
        assert_eq!(Errno::from(irq::IrqClaimError::LineBusy), Errno::EBUSY);
        assert_eq!(
            Errno::from(irq::IrqClaimError::DeviceHasNoIrq),
            Errno::ENODEV
        );
        assert_eq!(
            Errno::from(irq::IrqClaimError::MmioHandle(HandleError::WrongOwner)),
            Errno::EACCES
        );
        assert_eq!(
            Errno::from(irq::IrqError::Handle(HandleError::Stale)),
            Errno::ESTALE
        );
        assert_eq!(Errno::from(irq::IrqError::NoDelivery), Errno::EINVAL);
        assert_eq!(Errno::from(irq::IrqError::NotPolled), Errno::EINVAL);
        assert_eq!(
            Errno::from(dma::DmaError::Handle(HandleError::Stale)),
            Errno::ESTALE
        );
        assert_eq!(
            Errno::from(dma::DmaError::Mmio(mmio::MmioError::Unaligned)),
            Errno::EINVAL
        );
        assert_eq!(Errno::from(dma::DmaError::InvalidSize), Errno::EINVAL);
        assert_eq!(Errno::from(dma::DmaError::Exhausted), Errno::ENOMEM);
    }

    #[test]
    fn status_maps_ok_and_err() {
        assert_eq!(status(Ok::<(), HandleError>(())), 0);
        assert_eq!(status(Err::<(), HandleError>(HandleError::Invalid)), -9);
    }

    /// ABI 错误码数字**稳定不变**：进入 public ABI 后这些值是契约，测试即锚点。
    #[test]
    fn abi_error_codes_are_pinned_to_stable_numbers() {
        assert_eq!(Errno::ENOENT.code(), -2);
        assert_eq!(Errno::EIO.code(), -5);
        assert_eq!(Errno::ENOMEM.code(), -12);
        assert_eq!(Errno::EBUSY.code(), -16);
        assert_eq!(Errno::EINVAL.code(), -22);
        assert_eq!(Errno::ESTALE.code(), -116);
        assert_eq!(Errno::EKEYREVOKED.code(), -128);
    }

    // —— 每个错误枚举的**全部**变体 → Errno 映射 ——
    //
    // 每个测试里的 `match` 都不带通配臂：枚举新增变体时这里必须同步更新，
    // 否则无法编译——"映射表漏测"从"靠人记得"变成"编译器强制"。

    #[test]
    fn every_handle_error_arm_maps_to_its_pinned_errno() {
        for error in [
            HandleError::Invalid,
            HandleError::Stale,
            HandleError::WrongOwner,
            HandleError::Revoked,
            HandleError::AlreadyReleased,
        ] {
            let expected = match error {
                HandleError::Invalid => Errno::EBADF,
                HandleError::Stale => Errno::ESTALE,
                HandleError::WrongOwner => Errno::EACCES,
                HandleError::Revoked => Errno::EKEYREVOKED,
                HandleError::AlreadyReleased => Errno::EALREADY,
            };
            assert_eq!(Errno::from(error), expected, "HandleError {error:?}");
        }
    }

    #[test]
    fn every_task_error_arm_maps_to_its_pinned_errno() {
        for error in [
            TaskError::AlreadyExists,
            TaskError::NotFound,
            TaskError::NoMemory,
            TaskError::InvalidTransition,
            TaskError::RequesterNotFound,
            TaskError::RequesterNotReady,
            TaskError::WrongOwner,
            TaskError::EntryOutOfImage,
        ] {
            let expected = match &error {
                TaskError::AlreadyExists => Errno::EEXIST,
                TaskError::NotFound => Errno::ESRCH,
                TaskError::NoMemory => Errno::ENOMEM,
                TaskError::InvalidTransition => Errno::EINVAL,
                TaskError::RequesterNotFound => Errno::ESRCH,
                TaskError::RequesterNotReady => Errno::EAGAIN,
                TaskError::WrongOwner => Errno::EACCES,
                TaskError::EntryOutOfImage => Errno::EFAULT,
            };
            assert_eq!(Errno::from(error), expected, "TaskError 映射不符");
        }
    }

    #[test]
    fn every_sched_error_arm_maps_to_its_pinned_errno() {
        for error in [
            SchedError::NoPolicy,
            SchedError::InvalidTransition,
            SchedError::NotFound,
            SchedError::NoCurrent,
        ] {
            let expected = match error {
                SchedError::NoPolicy => Errno::ENOTSUP,
                SchedError::InvalidTransition => Errno::EINVAL,
                SchedError::NotFound => Errno::ESRCH,
                SchedError::NoCurrent => Errno::ESRCH,
            };
            assert_eq!(Errno::from(error), expected, "SchedError {error:?}");
        }
    }

    #[test]
    fn every_interface_error_arm_maps_to_its_pinned_errno() {
        for error in [
            InterfaceError::ProviderNotFound,
            InterfaceError::ProviderNotReady,
            InterfaceError::UnknownInterface,
            InterfaceError::KindMismatch,
            InterfaceError::AbiMismatch,
            InterfaceError::Unbound,
            InterfaceError::BindingNotFound,
            InterfaceError::IdExhausted,
        ] {
            let expected = match error {
                InterfaceError::ProviderNotFound => Errno::ESRCH,
                InterfaceError::ProviderNotReady => Errno::EAGAIN,
                InterfaceError::UnknownInterface => Errno::ENOENT,
                InterfaceError::KindMismatch => Errno::EINVAL,
                InterfaceError::AbiMismatch => Errno::EINVAL,
                InterfaceError::Unbound => Errno::ENOENT,
                InterfaceError::BindingNotFound => Errno::ENOENT,
                InterfaceError::IdExhausted => Errno::EOVERFLOW,
            };
            assert_eq!(Errno::from(error), expected, "InterfaceError {error:?}");
        }
    }

    #[test]
    fn every_component_load_error_arm_maps_to_its_pinned_errno() {
        use crate::component::loader::LoaderError;
        for error in [
            ComponentLoadError::StoreNotMounted,
            ComponentLoadError::NotFound,
            ComponentLoadError::ReadFailed,
            ComponentLoadError::Loader(LoaderError::BadMagic),
            ComponentLoadError::DeclareFailed,
            ComponentLoadError::ResolveFailed,
            ComponentLoadError::StartFailed,
            ComponentLoadError::CreateFailed(1),
            ComponentLoadError::CreatePanicked,
            ComponentLoadError::DestroyFailed(1),
            ComponentLoadError::DestroyPanicked,
            ComponentLoadError::InterfaceCommitFailed(InterfaceError::ProviderNotFound),
            ComponentLoadError::TaskPanicked(crate::task::TaskId::from_raw(1)),
        ] {
            let expected = match error {
                ComponentLoadError::StoreNotMounted => Errno::ENODEV,
                ComponentLoadError::NotFound => Errno::ENOENT,
                ComponentLoadError::ReadFailed => Errno::EIO,
                ComponentLoadError::Loader(_) => Errno::ENOEXEC,
                ComponentLoadError::ImageFailed => Errno::EEXIST,
                ComponentLoadError::DeclareFailed => Errno::EEXIST,
                ComponentLoadError::ResolveFailed => Errno::ENOENT,
                ComponentLoadError::StartFailed => Errno::EIO,
                ComponentLoadError::CreateFailed(_) => Errno::EIO,
                ComponentLoadError::CreatePanicked => Errno::EIO,
                ComponentLoadError::DestroyFailed(_) => Errno::EIO,
                ComponentLoadError::DestroyPanicked => Errno::EIO,
                ComponentLoadError::InterfaceCommitFailed(_) => Errno::EINVAL,
                ComponentLoadError::TaskPanicked(_) => Errno::EIO,
            };
            assert_eq!(Errno::from(error), expected, "ComponentLoadError {error:?}");
        }
    }

    #[test]
    fn every_component_stop_error_arm_maps_to_its_pinned_errno() {
        for error in [
            ComponentStopError::NotFound,
            ComponentStopError::NotReady,
            ComponentStopError::OwnsLiveTasks,
            ComponentStopError::DestroyFailed(1),
            ComponentStopError::DestroyPanicked,
            ComponentStopError::StateRejected,
        ] {
            let expected = match error {
                ComponentStopError::NotFound => Errno::ENOENT,
                ComponentStopError::NotReady => Errno::EINVAL,
                ComponentStopError::OwnsLiveTasks => Errno::EBUSY,
                ComponentStopError::DestroyFailed(_) => Errno::EIO,
                ComponentStopError::DestroyPanicked => Errno::EIO,
                ComponentStopError::StateRejected => Errno::EIO,
            };
            assert_eq!(Errno::from(error), expected, "ComponentStopError {error:?}");
        }
    }

    #[test]
    fn every_device_lookup_error_arm_maps_to_its_pinned_errno() {
        for error in [
            machine::DeviceLookupError::NoMachineInfo,
            machine::DeviceLookupError::NoSuchOrdinal,
        ] {
            let expected = match error {
                machine::DeviceLookupError::NoMachineInfo => Errno::ENODEV,
                machine::DeviceLookupError::NoSuchOrdinal => Errno::ENOENT,
            };
            assert_eq!(Errno::from(error), expected, "DeviceLookupError {error:?}");
        }
    }

    #[test]
    fn every_mmio_claim_error_arm_maps_to_its_pinned_errno() {
        for error in [
            mmio::MmioClaimError::DeviceNotFound,
            mmio::MmioClaimError::NotMmio,
            mmio::MmioClaimError::DeviceBusy,
            mmio::MmioClaimError::Denied,
        ] {
            let expected = match error {
                mmio::MmioClaimError::DeviceNotFound => Errno::ENODEV,
                mmio::MmioClaimError::NotMmio => Errno::ENOTSUP,
                mmio::MmioClaimError::DeviceBusy => Errno::EBUSY,
                mmio::MmioClaimError::Denied => Errno::EPERM,
            };
            assert_eq!(Errno::from(error), expected, "MmioClaimError {error:?}");
        }
    }

    /// `MmioError::Handle` 委托给 `HandleError` 的映射表（不重复定档）。
    #[test]
    fn every_mmio_error_arm_maps_to_its_pinned_errno() {
        for error in [
            mmio::MmioError::Handle(HandleError::Stale),
            mmio::MmioError::OutOfBounds,
            mmio::MmioError::Unaligned,
            mmio::MmioError::HasChildren,
        ] {
            let expected = match error {
                mmio::MmioError::Handle(inner) => Errno::from(inner),
                mmio::MmioError::OutOfBounds => Errno::EINVAL,
                mmio::MmioError::Unaligned => Errno::EINVAL,
                mmio::MmioError::HasChildren => Errno::EBUSY,
            };
            assert_eq!(Errno::from(error), expected, "MmioError {error:?}");
        }
    }

    /// `DmaError::Handle` / `DmaError::Mmio` 逐层委托到最内层映射。
    #[test]
    fn every_dma_error_arm_maps_to_its_pinned_errno() {
        for error in [
            dma::DmaError::Handle(HandleError::WrongOwner),
            dma::DmaError::Mmio(mmio::MmioError::Unaligned),
            dma::DmaError::InvalidSize,
            dma::DmaError::Exhausted,
        ] {
            let expected = match error {
                dma::DmaError::Handle(inner) => Errno::from(inner),
                dma::DmaError::Mmio(inner) => Errno::from(inner),
                dma::DmaError::InvalidSize => Errno::EINVAL,
                dma::DmaError::Exhausted => Errno::ENOMEM,
            };
            assert_eq!(Errno::from(error), expected, "DmaError {error:?}");
        }
    }

    #[test]
    fn every_irq_claim_error_arm_maps_to_its_pinned_errno() {
        for error in [
            irq::IrqClaimError::DeviceNotFound,
            irq::IrqClaimError::DeviceHasNoIrq,
            irq::IrqClaimError::LineBusy,
            irq::IrqClaimError::MmioHandle(HandleError::Invalid),
            irq::IrqClaimError::Denied,
        ] {
            let expected = match error {
                irq::IrqClaimError::DeviceNotFound => Errno::ENODEV,
                irq::IrqClaimError::DeviceHasNoIrq => Errno::ENODEV,
                irq::IrqClaimError::LineBusy => Errno::EBUSY,
                irq::IrqClaimError::MmioHandle(inner) => Errno::from(inner),
                irq::IrqClaimError::Denied => Errno::EPERM,
            };
            assert_eq!(Errno::from(error), expected, "IrqClaimError {error:?}");
        }
    }

    #[test]
    fn every_irq_error_arm_maps_to_its_pinned_errno() {
        for error in [
            irq::IrqError::Handle(HandleError::Revoked),
            irq::IrqError::NoDelivery,
            irq::IrqError::NotPolled,
        ] {
            let expected = match error {
                irq::IrqError::Handle(inner) => Errno::from(inner),
                irq::IrqError::NoDelivery => Errno::EINVAL,
                irq::IrqError::NotPolled => Errno::EINVAL,
            };
            assert_eq!(Errno::from(error), expected, "IrqError {error:?}");
        }
    }
}
