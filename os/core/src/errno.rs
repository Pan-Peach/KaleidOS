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
//!   `SchedError` / `EndpointError` / `DeviceClaimError` / `IrqError` / `DmaError`
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
            // 选择策略时 endpoint 校验失败：沿用 EndpointError 档位。
            SchedError::PolicyEndpoint(error) => Errno::from(error),
            // provider 没有 dispatcher：能力缺失（不是 I/O 错误）。
            SchedError::NoDispatcher => Errno::ENOSYS,
            // Core 无法准备策略执行栈：资源耗尽，策略配置不变。
            SchedError::NoPolicyStack => Errno::ENOMEM,
            // 策略 provider 不在 KernelNative 域：没有已实现的执行路径（ENOTSUP）。
            SchedError::PolicyUnsupportedDomain => Errno::ENOTSUP,
            SchedError::PolicyBusy => Errno::EBUSY,
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

impl From<crate::component::endpoint::BindError> for Errno {
    fn from(error: crate::component::endpoint::BindError) -> Self {
        use crate::component::endpoint::BindError;
        match error {
            // endpoint 校验失败：沿用 EndpointError 档位（EINVAL / ENOENT / ENODEV）。
            BindError::Endpoint(error) => Errno::from(error),
            // 组合 (caller, provider) 没有已实现机制：能力缺失，绝不静默降级。
            BindError::UnsupportedMechanism => Errno::ENOTSUP,
            // 选中 Direct 但 provider 没交付 function table：该 provider 服务不了
            // 这个机制（同样是"机制不可交付"档）。
            BindError::DirectWithoutApi => Errno::ENOTSUP,
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
            // provider 没有 dispatcher：组件不提供 endpoint 服务（能力缺失）。
            CallError::NoDispatcher => Errno::ENOSYS,
            // 重入：provider 已在当前同步链上（不是"稍后重试"，是同步环）。
            CallError::Reentrant => Errno::EBUSY,
            // 上下文种类拒绝：IRQ 回调不得发起通用服务调用（与 IRQ 内调度同档）。
            CallError::InIrqContext => Errno::EINVAL,
            // 上下文种类拒绝：policy 回调内不得发起通用 endpoint 调用。
            CallError::InPolicyContext => Errno::EINVAL,
            // 保留契约（scheduler.policy）：调度策略不是普通服务，Core 拒绝。
            CallError::ReservedContract => Errno::EPERM,
            // Core 侧资源耗尽（per-call service stack）：provider 从未执行。
            CallError::NoServiceStack => Errno::ENOMEM,
            // provider 已逻辑死亡（panic containment 已提交 Failed + 失效 endpoint）。
            CallError::ProviderFailed => Errno::EIO,
            // Isolated caller 的出站调用：跨 AS Gate 未实现 = 能力缺失，不是 I/O 错误。
            CallError::UnsupportedCallerDomain => Errno::ENOTSUP,
            // provider 域没有已实现的 dispatch 机制（Sandboxed 未实现）：能力缺失。
            CallError::UnsupportedProviderDomain => Errno::ENOTSUP,
        }
    }
}

impl From<ComponentLoadError> for Errno {
    fn from(error: ComponentLoadError) -> Self {
        match error {
            ComponentLoadError::StoreNotMounted => Errno::ENODEV,
            ComponentLoadError::NotFound => Errno::ENOENT,
            ComponentLoadError::ReadFailed => Errno::EIO,
            ComponentLoadError::Loader(crate::component::loader::LoaderError::OutOfMemory) => {
                Errno::ENOMEM
            }
            ComponentLoadError::Loader(_) => Errno::ENOEXEC,
            ComponentLoadError::DeclareFailed => Errno::EEXIST,
            ComponentLoadError::ResolveFailed => Errno::ENOENT,
            ComponentLoadError::StartFailed => Errno::EIO,
            // `CreateFailed` 的真实 errno 由 `ComponentLoadError::abi_status()` 在
            // ABI 边界保留（`Errno` 无法表达任意 raw code，这里 EIO 只是兜底：
            // 见 `create_failure_preserves_the_component_errno_at_the_abi_boundary`）。
            ComponentLoadError::CreateFailed(_) => Errno::EIO,
            ComponentLoadError::CreatePanicked => Errno::EIO,
            ComponentLoadError::DestroyFailed(_) => Errno::EIO,
            ComponentLoadError::DestroyPanicked => Errno::EIO,
            ComponentLoadError::EndpointCommitFailed(error) => Errno::from(error),
            ComponentLoadError::TaskPanicked(_) => Errno::EIO,
            ComponentLoadError::ServicePanicked => Errno::EIO,
            // 调度策略失败（panic / 非法提议 / 非 0 返回）：与其它组件失败同档。
            ComponentLoadError::PolicyPanicked => Errno::EIO,
            ComponentLoadError::PolicyRejected => Errno::EIO,
            // 上下文种类拒绝：policy 回调内不得创建组件（与调度拒绝同档）。
            ComponentLoadError::InPolicyContext | ComponentLoadError::InIrqContext => Errno::EINVAL,
            ComponentLoadError::CallerNotReady => Errno::EPERM,
            // 部署能力不足 / Isolated import 白名单外符号：能力缺失（不是 I/O
            // 错误）。都必须在 ABI 边界区分于 EIO，调用方才不会误判为可重试的
            // I/O。
            ComponentLoadError::IsolationUnsupported
            | ComponentLoadError::SandboxUnsupported
            | ComponentLoadError::IsolatedImportUnsupported => Errno::ENOTSUP,
            // 按域放段失败 / config 负载不合规：镜像 / 请求不适配该域（EINVAL）。
            ComponentLoadError::IsolatedPlacementFailed
            | ComponentLoadError::IsolatedConfigRejected => Errno::EINVAL,
            // 组件在私有 AS 内故障（Core 放弃实例）：与其它组件失败同档。
            ComponentLoadError::CreateFaulted => Errno::EIO,
            // provider 在跨 AS service 边界内故障（Core 放弃实例）：与 panic 同档。
            ComponentLoadError::ServiceFaulted => Errno::EIO,
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
            ComponentStopError::OwnsLiveTasks
            | ComponentStopError::ActiveExecutions
            | ComponentStopError::DirectExports => Errno::EBUSY,
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
            device::DeviceClaimError::OwnerNotReady => Errno::EPERM,
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
            dma::DmaError::OwnerNotReady => Errno::EPERM,
            dma::DmaError::IdExhausted => Errno::EOVERFLOW,
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
            irq::IrqError::OwnerNotReady => Errno::EPERM,
            irq::IrqError::NoHandler => Errno::EINVAL,
            irq::IrqError::LineBusy => Errno::EBUSY,
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
        // endpoint 臂展开到 `EndpointError` 的**全部**变体（无通配臂：新增变体 =
        // 编译错误；策略选择的失败沿用 endpoint 档位）。
        for endpoint_error in [
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
            let expected = match endpoint_error {
                EndpointError::EndpointNotFound | EndpointError::EndpointDead => Errno::ENOENT,
                EndpointError::ProviderNotFound => Errno::ENODEV,
                EndpointError::ProviderNotReady => Errno::EBUSY,
                EndpointError::ContractMismatch
                | EndpointError::KindMismatch
                | EndpointError::AbiMismatch => Errno::EINVAL,
                EndpointError::DuplicatePort => Errno::EEXIST,
                EndpointError::IdExhausted => Errno::ENOSPC,
            };
            assert_eq!(
                Errno::from(SchedError::PolicyEndpoint(endpoint_error)),
                expected,
                "SchedError::PolicyEndpoint({endpoint_error:?})"
            );
        }

        for error in [
            SchedError::NoPolicy,
            SchedError::InvalidTransition,
            SchedError::NotFound,
            SchedError::NoCurrent,
            SchedError::NoDispatcher,
            SchedError::NoPolicyStack,
            SchedError::PolicyUnsupportedDomain,
            SchedError::PolicyBusy,
        ] {
            let expected = match error {
                SchedError::NoPolicy => Errno::ENOTSUP,
                SchedError::InvalidTransition => Errno::EINVAL,
                SchedError::NotFound => Errno::ESRCH,
                SchedError::NoCurrent => Errno::ESRCH,
                SchedError::NoDispatcher => Errno::ENOSYS,
                SchedError::NoPolicyStack => Errno::ENOMEM,
                // 策略 provider 不在 KernelNative 域：没有已实现的执行路径。
                SchedError::PolicyUnsupportedDomain => Errno::ENOTSUP,
                SchedError::PolicyBusy => Errno::EBUSY,
                // endpoint 臂已在上面的循环里逐变体覆盖。
                SchedError::PolicyEndpoint(_) => unreachable!("covered above"),
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
            CallError::NoDispatcher,
            CallError::Reentrant,
            CallError::InIrqContext,
            CallError::InPolicyContext,
            CallError::ReservedContract,
            CallError::NoServiceStack,
            CallError::ProviderFailed,
            CallError::UnsupportedCallerDomain,
            CallError::UnsupportedProviderDomain,
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
                CallError::NoDispatcher => Errno::ENOSYS,
                CallError::Reentrant => Errno::EBUSY,
                CallError::InIrqContext => Errno::EINVAL,
                CallError::InPolicyContext => Errno::EINVAL,
                CallError::ReservedContract => Errno::EPERM,
                CallError::NoServiceStack => Errno::ENOMEM,
                CallError::ProviderFailed => Errno::EIO,
                CallError::UnsupportedCallerDomain => Errno::ENOTSUP,
                CallError::UnsupportedProviderDomain => Errno::ENOTSUP,
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
    fn every_component_load_error_arm_maps_to_its_pinned_errno() {
        use crate::component::loader::LoaderError;
        for error in [
            ComponentLoadError::StoreNotMounted,
            ComponentLoadError::NotFound,
            ComponentLoadError::ReadFailed,
            ComponentLoadError::Loader(LoaderError::BadMagic),
            ComponentLoadError::Loader(LoaderError::OutOfMemory),
            ComponentLoadError::DeclareFailed,
            ComponentLoadError::ResolveFailed,
            ComponentLoadError::StartFailed,
            ComponentLoadError::CreateFailed(1),
            ComponentLoadError::CreatePanicked,
            ComponentLoadError::DestroyFailed(1),
            ComponentLoadError::DestroyPanicked,
            ComponentLoadError::EndpointCommitFailed(EndpointError::DuplicatePort),
            ComponentLoadError::TaskPanicked(crate::task::TaskId::from_raw(1)),
            ComponentLoadError::ServicePanicked,
            ComponentLoadError::PolicyPanicked,
            ComponentLoadError::PolicyRejected,
            ComponentLoadError::InPolicyContext,
            ComponentLoadError::InIrqContext,
            ComponentLoadError::CallerNotReady,
            ComponentLoadError::IsolationUnsupported,
            ComponentLoadError::SandboxUnsupported,
            ComponentLoadError::IsolatedImportUnsupported,
            ComponentLoadError::IsolatedPlacementFailed,
            ComponentLoadError::IsolatedConfigRejected,
            ComponentLoadError::CreateFaulted,
            ComponentLoadError::ServiceFaulted,
        ] {
            let expected = match error {
                ComponentLoadError::StoreNotMounted => Errno::ENODEV,
                ComponentLoadError::NotFound => Errno::ENOENT,
                ComponentLoadError::ReadFailed => Errno::EIO,
                ComponentLoadError::Loader(LoaderError::OutOfMemory) => Errno::ENOMEM,
                ComponentLoadError::Loader(_) => Errno::ENOEXEC,
                ComponentLoadError::DeclareFailed => Errno::EEXIST,
                ComponentLoadError::ResolveFailed => Errno::ENOENT,
                ComponentLoadError::StartFailed => Errno::EIO,
                ComponentLoadError::CreateFailed(_) => Errno::EIO,
                ComponentLoadError::CreatePanicked => Errno::EIO,
                ComponentLoadError::DestroyFailed(_) => Errno::EIO,
                ComponentLoadError::DestroyPanicked => Errno::EIO,
                ComponentLoadError::EndpointCommitFailed(EndpointError::DuplicatePort) => {
                    Errno::EEXIST
                }
                ComponentLoadError::EndpointCommitFailed(_) => Errno::EINVAL,
                ComponentLoadError::TaskPanicked(_) => Errno::EIO,
                ComponentLoadError::ServicePanicked => Errno::EIO,
                ComponentLoadError::PolicyPanicked => Errno::EIO,
                ComponentLoadError::PolicyRejected => Errno::EIO,
                ComponentLoadError::InPolicyContext | ComponentLoadError::InIrqContext => {
                    Errno::EINVAL
                }
                ComponentLoadError::CallerNotReady => Errno::EPERM,
                ComponentLoadError::IsolationUnsupported
                | ComponentLoadError::SandboxUnsupported
                | ComponentLoadError::IsolatedImportUnsupported => Errno::ENOTSUP,
                ComponentLoadError::IsolatedPlacementFailed
                | ComponentLoadError::IsolatedConfigRejected => Errno::EINVAL,
                ComponentLoadError::CreateFaulted => Errno::EIO,
                ComponentLoadError::ServiceFaulted => Errno::EIO,
            };
            assert_eq!(Errno::from(error), expected, "ComponentLoadError {error:?}");
        }
    }

    /// create 入口的原始 errno 必须在 ABI 边界**保留**：driver prober 的创建失败
    /// 记录需要真实原因（`-EBUSY` / `-EINVAL`），不能塌缩成 `EIO`。
    #[test]
    fn create_failure_preserves_the_component_errno_at_the_abi_boundary() {
        use crate::component::loader::LoaderError;
        for (code, expected) in [
            (Errno::ENODEV.code(), Errno::ENODEV.code()),
            (Errno::EBUSY.code(), Errno::EBUSY.code()),
            (Errno::EINVAL.code(), Errno::EINVAL.code()),
            (Errno::ENOMEM.code(), Errno::ENOMEM.code()),
        ] {
            assert_eq!(
                ComponentLoadError::CreateFailed(code).abi_status(),
                expected,
                "CreateFailed({code}) 必须原样透传"
            );
        }
        // 正数非零 = 组件违反 `0 / -errno` 约定 → EIO（不是有意义的 errno）。
        assert_eq!(
            ComponentLoadError::CreateFailed(7).abi_status(),
            Errno::EIO.code()
        );
        // 其余臂沿用 `From` 映射（边界状态码是同一张表 + create 透传）。
        assert_eq!(
            ComponentLoadError::NotFound.abi_status(),
            Errno::ENOENT.code()
        );
        assert_eq!(
            ComponentLoadError::Loader(LoaderError::BadMagic).abi_status(),
            Errno::ENOEXEC.code()
        );
        assert_eq!(
            ComponentLoadError::CreatePanicked.abi_status(),
            Errno::EIO.code()
        );
    }

    #[test]
    fn every_component_stop_error_arm_maps_to_its_pinned_errno() {
        for error in [
            ComponentStopError::NotFound,
            ComponentStopError::NotReady,
            ComponentStopError::OwnsLiveTasks,
            ComponentStopError::ActiveExecutions,
            ComponentStopError::DirectExports,
            ComponentStopError::DestroyFailed(1),
            ComponentStopError::DestroyPanicked,
            ComponentStopError::StateRejected,
        ] {
            let expected = match error {
                ComponentStopError::NotFound => Errno::ENOENT,
                ComponentStopError::NotReady => Errno::EINVAL,
                ComponentStopError::OwnsLiveTasks
                | ComponentStopError::ActiveExecutions
                | ComponentStopError::DirectExports => Errno::EBUSY,
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
            device::DeviceClaimError::OwnerNotReady,
            device::DeviceClaimError::DeviceBusy,
        ] {
            let expected = match error {
                device::DeviceClaimError::DeviceNotFound => Errno::ENODEV,
                device::DeviceClaimError::NotMmio => Errno::ENOTSUP,
                device::DeviceClaimError::OwnerNotReady => Errno::EPERM,
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
            dma::DmaError::OwnerNotReady,
            dma::DmaError::IdExhausted,
            dma::DmaError::DeviceNotFound,
            dma::DmaError::NotOwner,
            dma::DmaError::NotFound,
            dma::DmaError::BadRange,
        ] {
            let expected = match error {
                dma::DmaError::InvalidSize | dma::DmaError::BadRange => Errno::EINVAL,
                dma::DmaError::Exhausted => Errno::ENOMEM,
                dma::DmaError::OwnerNotReady => Errno::EPERM,
                dma::DmaError::IdExhausted => Errno::EOVERFLOW,
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
            irq::IrqError::OwnerNotReady,
            irq::IrqError::NoHandler,
            irq::IrqError::LineBusy,
        ] {
            let expected = match error {
                irq::IrqError::DeviceNotFound | irq::IrqError::NoIrq => Errno::ENODEV,
                irq::IrqError::NotOwner => Errno::EACCES,
                irq::IrqError::OwnerNotReady => Errno::EPERM,
                irq::IrqError::NoHandler => Errno::EINVAL,
                irq::IrqError::LineBusy => Errno::EBUSY,
            };
            assert_eq!(Errno::from(error), expected, "IrqError {error:?}");
        }
    }
}
