//! Core ABI 错误码命名空间（Errno）：内部错误 → 跨边界错误码的唯一翻译点。
//!
//! # ABI 错误约定（v3 起）
//!
//! ```text
//! 0          success
//! -negative  failure: -Errno
//! ```
//!
//! - **内部保持丰富**：`TaskError` / `ComponentLoadError` /
//!   `SchedError` / `InterfaceError` / `DeviceClaimError` / `IrqError` / `DmaError`
//!   等继续各自为政（强类型、可重构），只在 Core ABI 边界翻译成 `Errno`——
//!   本文件是唯一映射表。
//! - **数值稳定**：Linux/POSIX 风格；枚举本体由 `tools/kabi/kabi_gen.py` 从
//!   `abi/errno.toml` 生成到 [`crate::generated::errno`]（Core / SDK / C 同源），
//!   进入 public ABI 后数字不再变更。
//! - 组件（Rust / C / Wasm / IPC）只需要理解这一套错误码。
//! - 有返回值的 action 统一 `i32 status + out 参数`（`0` / `-errno`），
//!   不再新增"正数成功 / 负数错误"协议；纯 query 与 allocator 风格 API
//!   不强制（见 `component/export.rs`）。

use crate::component::call::CallError;
use crate::component::endpoint::EndpointError;
use crate::component::exit::ComponentStopError;
use crate::component::interface::InterfaceError;
use crate::component::load::ComponentLoadError;
use crate::machine;
use crate::resource::{device, dma, irq};
use crate::sched::SchedError;
use crate::task::TaskError;

pub use crate::generated::errno::Errno;

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

impl From<EndpointError> for Errno {
    fn from(error: EndpointError) -> Self {
        match error {
            // 未发布 / 已死：不交付死 endpoint（与 `Unbound` 同一档）。
            EndpointError::EndpointNotFound | EndpointError::EndpointDead => Errno::ENOENT,
            // provider 记录本身不存在（身份消失）——与设备缺席同档。
            EndpointError::ProviderNotFound => Errno::ENODEV,
            // provider 存在但不在可发布状态（create 之外 / 已停止）。
            EndpointError::ProviderNotReady => Errno::EBUSY,
            // 契约 / ABI 是 exact match，对不上就是参数非法。
            EndpointError::ContractMismatch
            | EndpointError::KindMismatch
            | EndpointError::AbiMismatch => Errno::EINVAL,
            // 端口名在 provider 实例内唯一：重复发布拒绝，不重定向。
            EndpointError::DuplicatePort => Errno::EEXIST,
            // EndpointId 空间耗尽（u64 单调）。
            EndpointError::IdExhausted => Errno::ENOSPC,
        }
    }
}

impl From<CallError> for Errno {
    fn from(error: CallError) -> Self {
        match error {
            // 身份门禁：无 principal / caller 已 Failed（与其它 acquiring 入口同档）。
            CallError::NoCaller | CallError::CallerFailed => Errno::EPERM,
            // frame 结构非法（out_status 为空 / 长度非零配空指针）。
            CallError::InvalidFrame => Errno::EFAULT,
            // endpoint 存活解析失败（未发布 / 已死 / owner 消失）——沿用 EndpointError 档位。
            CallError::Endpoint(error) => Errno::from(error),
            // provider 不在 Ready / inflight 溢出：当前拒绝，稍后可能可用。
            CallError::ProviderBusy => Errno::EBUSY,
            // owner → image 引用断了（Core 不变式破坏，不应发生）：与设备缺席同档。
            CallError::ImageMissing => Errno::ENODEV,
            // provider 没有 dispatcher：组件不提供 endpoint 服务（能力缺失）。
            CallError::NoDispatcher => Errno::ENOSYS,
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
            ComponentLoadError::EndpointCommitFailed(error) => Errno::from(error),
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

impl From<device::DeviceClaimError> for Errno {
    fn from(error: device::DeviceClaimError) -> Self {
        match error {
            device::DeviceClaimError::DeviceNotFound => Errno::ENODEV,
            device::DeviceClaimError::NotMmio => Errno::ENOTSUP,
            device::DeviceClaimError::DeviceBusy => Errno::EBUSY,
        }
    }
}

impl From<device::DeviceReleaseError> for Errno {
    fn from(error: device::DeviceReleaseError) -> Self {
        match error {
            device::DeviceReleaseError::DeviceNotFound => Errno::ENODEV,
            device::DeviceReleaseError::NotOwner => Errno::EACCES,
            device::DeviceReleaseError::HasChildren => Errno::EBUSY,
        }
    }
}

impl From<dma::DmaError> for Errno {
    fn from(error: dma::DmaError) -> Self {
        match error {
            dma::DmaError::InvalidSize | dma::DmaError::BadRange => Errno::EINVAL,
            dma::DmaError::Exhausted => Errno::ENOMEM,
            dma::DmaError::DeviceNotFound => Errno::ENODEV,
            dma::DmaError::NotOwner => Errno::EACCES,
            dma::DmaError::NotFound => Errno::ENOENT,
        }
    }
}

impl From<irq::IrqError> for Errno {
    fn from(error: irq::IrqError) -> Self {
        match error {
            irq::IrqError::DeviceNotFound | irq::IrqError::NoIrq => Errno::ENODEV,
            irq::IrqError::NotOwner => Errno::EACCES,
            irq::IrqError::NoHandler => Errno::EINVAL,
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
        assert_eq!(Errno::from(TaskError::NoMemory), Errno::ENOMEM);
        assert_eq!(Errno::from(TaskError::InvalidTransition), Errno::EINVAL);
        assert_eq!(Errno::from(SchedError::NoPolicy), Errno::ENOTSUP);
        assert_eq!(Errno::from(InterfaceError::ProviderNotReady), Errno::EAGAIN);
        assert_eq!(Errno::from(EndpointError::EndpointDead), Errno::ENOENT);
        assert_eq!(Errno::from(EndpointError::ProviderNotReady), Errno::EBUSY);
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
        assert_eq!(
            Errno::from(machine::DeviceLookupError::NoMachineInfo),
            Errno::ENODEV
        );
        assert_eq!(
            Errno::from(machine::DeviceLookupError::NoSuchOrdinal),
            Errno::ENOENT
        );
        assert_eq!(
            Errno::from(device::DeviceClaimError::DeviceBusy),
            Errno::EBUSY
        );
        assert_eq!(
            Errno::from(device::DeviceReleaseError::HasChildren),
            Errno::EBUSY
        );
        assert_eq!(Errno::from(irq::IrqError::NotOwner), Errno::EACCES);
        assert_eq!(Errno::from(irq::IrqError::NoHandler), Errno::EINVAL);
        assert_eq!(Errno::from(dma::DmaError::NotFound), Errno::ENOENT);
        assert_eq!(Errno::from(dma::DmaError::InvalidSize), Errno::EINVAL);
        assert_eq!(Errno::from(dma::DmaError::Exhausted), Errno::ENOMEM);
    }

    #[test]
    fn status_maps_ok_and_err() {
        assert_eq!(status(Ok::<(), TaskError>(())), 0);
        assert_eq!(
            status(Err::<(), TaskError>(TaskError::NotFound)),
            Errno::ESRCH.code()
        );
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
    fn every_endpoint_error_arm_maps_to_its_pinned_errno() {
        for error in [
            EndpointError::EndpointNotFound,
            EndpointError::EndpointDead,
            EndpointError::ProviderNotFound,
            EndpointError::ProviderNotReady,
            EndpointError::ContractMismatch,
            EndpointError::KindMismatch,
            EndpointError::AbiMismatch,
            EndpointError::DuplicatePort,
            EndpointError::IdExhausted,
        ] {
            let expected = match error {
                EndpointError::EndpointNotFound | EndpointError::EndpointDead => Errno::ENOENT,
                EndpointError::ProviderNotFound => Errno::ENODEV,
                EndpointError::ProviderNotReady => Errno::EBUSY,
                EndpointError::ContractMismatch
                | EndpointError::KindMismatch
                | EndpointError::AbiMismatch => Errno::EINVAL,
                EndpointError::DuplicatePort => Errno::EEXIST,
                EndpointError::IdExhausted => Errno::ENOSPC,
            };
            assert_eq!(Errno::from(error), expected, "EndpointError {error:?}");
        }
    }

    /// `CallError` 的**全部**臂 → Errno（endpoint 臂展开到 `EndpointError` 的全部
    /// 变体：两边都不带通配臂，新增变体 = 编译错误）。
    #[test]
    fn every_call_error_arm_maps_to_its_pinned_errno() {
        for error in [
            CallError::NoCaller,
            CallError::CallerFailed,
            CallError::InvalidFrame,
            CallError::Endpoint(EndpointError::EndpointNotFound),
            CallError::Endpoint(EndpointError::EndpointDead),
            CallError::Endpoint(EndpointError::ProviderNotFound),
            CallError::Endpoint(EndpointError::ProviderNotReady),
            CallError::Endpoint(EndpointError::ContractMismatch),
            CallError::Endpoint(EndpointError::KindMismatch),
            CallError::Endpoint(EndpointError::AbiMismatch),
            CallError::Endpoint(EndpointError::DuplicatePort),
            CallError::Endpoint(EndpointError::IdExhausted),
            CallError::ProviderBusy,
            CallError::ImageMissing,
            CallError::NoDispatcher,
        ] {
            let expected = match error {
                CallError::NoCaller | CallError::CallerFailed => Errno::EPERM,
                CallError::InvalidFrame => Errno::EFAULT,
                CallError::Endpoint(
                    EndpointError::EndpointNotFound | EndpointError::EndpointDead,
                ) => Errno::ENOENT,
                CallError::Endpoint(EndpointError::ProviderNotFound) => Errno::ENODEV,
                CallError::Endpoint(EndpointError::ProviderNotReady) => Errno::EBUSY,
                CallError::Endpoint(
                    EndpointError::ContractMismatch
                    | EndpointError::KindMismatch
                    | EndpointError::AbiMismatch,
                ) => Errno::EINVAL,
                CallError::Endpoint(EndpointError::DuplicatePort) => Errno::EEXIST,
                CallError::Endpoint(EndpointError::IdExhausted) => Errno::ENOSPC,
                CallError::ProviderBusy => Errno::EBUSY,
                CallError::ImageMissing => Errno::ENODEV,
                CallError::NoDispatcher => Errno::ENOSYS,
            };
            assert_eq!(Errno::from(error), expected, "CallError {error:?}");
        }
        // "image 没有 dispatcher" 的文档化 errno：能力缺失 → ENOSYS（不是 EIO）。
        assert_eq!(
            Errno::from(CallError::NoDispatcher),
            Errno::ENOSYS,
            "无 dispatcher = 能力缺失，用 ENOSYS 而不是 I/O 错误"
        );
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
            ComponentLoadError::EndpointCommitFailed(EndpointError::DuplicatePort),
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
                ComponentLoadError::EndpointCommitFailed(EndpointError::DuplicatePort) => {
                    Errno::EEXIST
                }
                ComponentLoadError::EndpointCommitFailed(_) => Errno::EINVAL,
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

    // —— 每个错误枚举的**全部**变体 → Errno 映射 ——
    //
    // 每个测试里的 `match` 都不带通配臂：枚举新增变体时这里必须同步更新，
    // 否则无法编译——"映射表漏测"从"靠人记得"变成"编译器强制"。

    #[test]
    fn every_device_claim_error_arm_maps_to_its_pinned_errno() {
        for error in [
            device::DeviceClaimError::DeviceNotFound,
            device::DeviceClaimError::NotMmio,
            device::DeviceClaimError::DeviceBusy,
        ] {
            let expected = match error {
                device::DeviceClaimError::DeviceNotFound => Errno::ENODEV,
                device::DeviceClaimError::NotMmio => Errno::ENOTSUP,
                device::DeviceClaimError::DeviceBusy => Errno::EBUSY,
            };
            assert_eq!(Errno::from(error), expected, "DeviceClaimError {error:?}");
        }
    }

    #[test]
    fn every_device_release_error_arm_maps_to_its_pinned_errno() {
        for error in [
            device::DeviceReleaseError::DeviceNotFound,
            device::DeviceReleaseError::NotOwner,
            device::DeviceReleaseError::HasChildren,
        ] {
            let expected = match error {
                device::DeviceReleaseError::DeviceNotFound => Errno::ENODEV,
                device::DeviceReleaseError::NotOwner => Errno::EACCES,
                device::DeviceReleaseError::HasChildren => Errno::EBUSY,
            };
            assert_eq!(Errno::from(error), expected, "DeviceReleaseError {error:?}");
        }
    }

    #[test]
    fn every_dma_error_arm_maps_to_its_pinned_errno() {
        for error in [
            dma::DmaError::InvalidSize,
            dma::DmaError::Exhausted,
            dma::DmaError::DeviceNotFound,
            dma::DmaError::NotOwner,
            dma::DmaError::NotFound,
            dma::DmaError::BadRange,
        ] {
            let expected = match error {
                dma::DmaError::InvalidSize | dma::DmaError::BadRange => Errno::EINVAL,
                dma::DmaError::Exhausted => Errno::ENOMEM,
                dma::DmaError::DeviceNotFound => Errno::ENODEV,
                dma::DmaError::NotOwner => Errno::EACCES,
                dma::DmaError::NotFound => Errno::ENOENT,
            };
            assert_eq!(Errno::from(error), expected, "DmaError {error:?}");
        }
    }

    #[test]
    fn every_irq_error_arm_maps_to_its_pinned_errno() {
        for error in [
            irq::IrqError::DeviceNotFound,
            irq::IrqError::NoIrq,
            irq::IrqError::NotOwner,
            irq::IrqError::NoHandler,
        ] {
            let expected = match error {
                irq::IrqError::DeviceNotFound | irq::IrqError::NoIrq => Errno::ENODEV,
                irq::IrqError::NotOwner => Errno::EACCES,
                irq::IrqError::NoHandler => Errno::EINVAL,
            };
            assert_eq!(Errno::from(error), expected, "IrqError {error:?}");
        }
    }
}
