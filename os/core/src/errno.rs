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
//! - **数值稳定**：Linux/POSIX 风格；进入 public ABI 后，数字不再变更。
//! - 组件（Rust / C / Wasm / IPC）只需要理解这一套错误码。
//! - 有返回值的 action 统一 `i32 status + out 参数`（`0` / `-errno`），
//!   不再新增"正数成功 / 负数错误"协议；纯 query 与 allocator 风格 API
//!   不强制（见 `component/export.rs`）。

use crate::component::exit::ComponentStopError;
use crate::component::interface::InterfaceError;
use crate::component::load::ComponentLoadError;
use crate::machine;
use crate::resource::{device, dma, irq};
use crate::sched::SchedError;
use crate::task::TaskError;

/// 稳定、Linux/POSIX 风格的 ABI 错误码。**完整的 `asm-generic/errno` 集合**
/// （1–133；未使用的 41 / 58 留空；95 的 POSIX 别名 `EOPNOTSUPP` 与
/// [`Errno::ENOTSUP`] 同码，不再单列）。进入 public ABI 后数字不再变更；
/// 与 SDK `src/errno.rs` / C `include/errno.h` 三方由 drift test 钉死。
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Errno {
    EPERM = 1,
    ENOENT = 2,
    ESRCH = 3,
    EINTR = 4,
    EIO = 5,
    ENXIO = 6,
    E2BIG = 7,
    ENOEXEC = 8,
    EBADF = 9,
    ECHILD = 10,
    EAGAIN = 11,
    ENOMEM = 12,
    EACCES = 13,
    EFAULT = 14,
    ENOTBLK = 15,
    EBUSY = 16,
    EEXIST = 17,
    EXDEV = 18,
    ENODEV = 19,
    ENOTDIR = 20,
    EISDIR = 21,
    EINVAL = 22,
    ENFILE = 23,
    EMFILE = 24,
    ENOTTY = 25,
    ETXTBSY = 26,
    EFBIG = 27,
    ENOSPC = 28,
    ESPIPE = 29,
    EROFS = 30,
    EMLINK = 31,
    EPIPE = 32,
    EDOM = 33,
    ERANGE = 34,
    EDEADLK = 35,
    ENAMETOOLONG = 36,
    ENOLCK = 37,
    ENOSYS = 38,
    ENOTEMPTY = 39,
    ELOOP = 40,
    ENOMSG = 42,
    EIDRM = 43,
    ECHRNG = 44,
    EL2NSYNC = 45,
    EL3HLT = 46,
    EL3RST = 47,
    ELNRNG = 48,
    EUNATCH = 49,
    ENOCSI = 50,
    EL2HLT = 51,
    EBADE = 52,
    EBADR = 53,
    EXFULL = 54,
    ENOANO = 55,
    EBADRQC = 56,
    EBADSLT = 57,
    EBFONT = 59,
    ENOSTR = 60,
    ENODATA = 61,
    ETIME = 62,
    ENOSR = 63,
    ENONET = 64,
    ENOPKG = 65,
    EREMOTE = 66,
    ENOLINK = 67,
    EADV = 68,
    ESRMNT = 69,
    ECOMM = 70,
    EPROTO = 71,
    EMULTIHOP = 72,
    EDOTDOT = 73,
    EBADMSG = 74,
    EOVERFLOW = 75,
    ENOTUNIQ = 76,
    EBADFD = 77,
    EREMCHG = 78,
    ELIBACC = 79,
    ELIBBAD = 80,
    ELIBSCN = 81,
    ELIBMAX = 82,
    ELIBEXEC = 83,
    EILSEQ = 84,
    ERESTART = 85,
    ESTRPIPE = 86,
    EUSERS = 87,
    ENOTSOCK = 88,
    EDESTADDRREQ = 89,
    EMSGSIZE = 90,
    EPROTOTYPE = 91,
    ENOPROTOOPT = 92,
    EPROTONOSUPPORT = 93,
    ESOCKTNOSUPPORT = 94,
    ENOTSUP = 95,
    EPFNOSUPPORT = 96,
    EAFNOSUPPORT = 97,
    EADDRINUSE = 98,
    EADDRNOTAVAIL = 99,
    ENETDOWN = 100,
    ENETUNREACH = 101,
    ENETRESET = 102,
    ECONNABORTED = 103,
    ECONNRESET = 104,
    ENOBUFS = 105,
    EISCONN = 106,
    ENOTCONN = 107,
    ESHUTDOWN = 108,
    ETOOMANYREFS = 109,
    ETIMEDOUT = 110,
    ECONNREFUSED = 111,
    EHOSTDOWN = 112,
    EHOSTUNREACH = 113,
    EALREADY = 114,
    EINPROGRESS = 115,
    ESTALE = 116,
    EUCLEAN = 117,
    ENOTNAM = 118,
    ENAVAIL = 119,
    EISNAM = 120,
    EREMOTEIO = 121,
    EDQUOT = 122,
    ENOMEDIUM = 123,
    EMEDIUMTYPE = 124,
    ECANCELED = 125,
    ENOKEY = 126,
    EKEYEXPIRED = 127,
    EKEYREVOKED = 128,
    EKEYREJECTED = 129,
    EOWNERDEAD = 130,
    ENOTRECOVERABLE = 131,
    ERFKILL = 132,
    EHWPOISON = 133,
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
