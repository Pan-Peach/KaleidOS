//! `scheduler.policy` 契约（KIND = Policy）：provider = 策略组件，**consumer =
//! Core 自己**。
//!
//! 这个契约与 block / filesystem / probe 不同，它**没有 Direct function table**：
//!
//! - **选择**（组合方）：`Endpoint::<SchedulerPolicy>::lookup(provider, name)`
//!   （Core 校验 contract + 存活）后 [`select`]（`kcore_sched_set_policy`）——Core
//!   只把 opaque `EndpointId` 提交为调度配置，**不发布任何东西**。必须在 provider
//!   的 create 返回 0 之后调用（endpoint 只在 staged publish 原子提交后存在）。
//! - **执行**（Core）：调度 commit 路径经 Core 私有的 PolicyCall 边界调用
//!   provider 的 `kcomp_service_dispatch`（Core 是 caller，无 caller principal /
//!   caller task）。组件**不得**经 `kcore_endpoint_call` 调这个契约——Core 的通用
//!   调用路径显式拒绝它（保留契约）。
//! - **provider 侧**：[`publish_endpoint`] 发布 Gate-only endpoint（`api` 永远为
//!   空；Direct 绑定会被 Core 以 `-ENOTSUP` 显式拒绝，绝不静默降级），并在 image
//!   里提供 `kcomp_service_dispatch`（`kcomp_services!`）。
//!
//! wire 格式（`abi/scheduler.toml` 的 CHOOSE_NEXT）：
//!
//! ```text
//! args   : current TaskId（u32 LE；KCOMP_SCHEDULER_NONE = 无）+ CpuId（u32 LE）
//! input  : runnable TaskId 列表（逐个 u32 LE 连接，非空）
//! output : 提议的 TaskId（u32 LE，恰好 4 字节）
//! 返回   : 0 / -errno
//! ```
//!
//! provider 的职责只有"提议"：Core 会验证提议落在 `input` 列表内；不在 → 提议被
//! 拒绝、provider 被隔离（逻辑死亡）。回调**不得阻塞、不得分配**；调度 / 通用
//! endpoint 调用 / 嵌套创建 / 策略替换在边界内一律被 Core 拒绝。

use crate::abi;
use crate::abi::InterfaceAbi;
use crate::endpoint::{Contract, Endpoint};
use crate::errno::{Errno, Result};
use crate::generated::abi::InterfaceKind;
use crate::generated::scheduler::{
    KCOMP_SCHEDULER_METHOD_CHOOSE_NEXT, KCOMP_SCHEDULER_NONE, KCOMP_SCHEDULER_POLICY_ABI,
    KCOMP_SCHEDULER_POLICY_CONTRACT, KCOMP_SCHEDULER_POLICY_NAME, KCOMP_SCHEDULER_TASK_ID_LEN,
};

/// `scheduler.policy` 的 endpoint 端口名（组合期 discover 用；provider 实例内唯一）。
pub const SCHEDULER_POLICY_NAME: &[u8] = KCOMP_SCHEDULER_POLICY_NAME;

/// `scheduler.policy` 的 exact ABI fingerprint。
///
/// 数值 = 8 字节 ASCII tag `b"SCHEDCPU"` 的大端读数；raw `u64` 本体在生成物
/// （schema 单一来源），这里包成 [`InterfaceAbi`] newtype。
pub const SCHEDULER_POLICY_ABI: InterfaceAbi = InterfaceAbi::from_raw(KCOMP_SCHEDULER_POLICY_ABI);

/// `CHOOSE_NEXT` 的方法号（`kcomp_service_dispatch` 的 `method` 参数）。
pub const SCHEDULER_METHOD_CHOOSE_NEXT: u32 = KCOMP_SCHEDULER_METHOD_CHOOSE_NEXT;

/// `args` 里的"当前无任务"哨兵（`UINT32_MAX`）。
pub const SCHEDULER_NONE: u32 = KCOMP_SCHEDULER_NONE;

/// 一个 TaskId 在扁平 frame 里的编码长度（u32 LE）。
pub const SCHEDULER_TASK_ID_LEN: usize = KCOMP_SCHEDULER_TASK_ID_LEN;

/// `scheduler.policy` 契约（KIND = Policy）。
///
/// 只实现 [`Contract`]（Endpoint 模型）：调度策略没有 Direct function table。
pub struct SchedulerPolicy;

impl Contract for SchedulerPolicy {
    const ID: u64 = KCOMP_SCHEDULER_POLICY_CONTRACT;
    const ABI: u64 = KCOMP_SCHEDULER_POLICY_ABI;
    const KIND: InterfaceKind = InterfaceKind::Policy;
}

// -----------------------------------------------------------------------
// 组合方（consumer = Core 的代理）
// -----------------------------------------------------------------------

/// 把已校验的 `scheduler.policy` endpoint 提交为 Core 的调度配置
/// （`kcore_sched_set_policy`）。
///
/// 成功 = 该 provider 成为活动调度策略（Core 只记 `EndpointId` + 准备好的执行栈）；
/// 失败 = `-Errno`（未发布 / 已死 / provider 非 Ready / 契约不符 / provider 没有
/// `kcomp_service_dispatch` / Core 无法准备执行栈）。**不发布任何东西**。
pub fn select(endpoint: &Endpoint<SchedulerPolicy>) -> Result<()> {
    // SAFETY: Core 只读该 opaque id 并写自己的调度配置；无指针借用。
    let status = unsafe { abi::kcore_sched_set_policy(endpoint.id()) };
    if status == 0 {
        Ok(())
    } else {
        Err(Errno::from_code(status))
    }
}

// -----------------------------------------------------------------------
// provider 侧
// -----------------------------------------------------------------------

/// provider 侧：发布本实例的 `scheduler.policy` endpoint（**Gate-only**）。
///
/// `port` 是 provider 定义的不透明 dispatch token——Core 的 PolicyCall 边界经
/// image 的 `kcomp_service_dispatch` 用它选中本契约。没有 Direct function table：
/// `api` 永远为空（任何 Direct 绑定都会被 Core 显式拒绝）。发布是 staged 的：只在
/// `kcomp_instance_create` 期间有效，create 返回 0 后 Core 原子提交。
///
/// 安全 fn：发布的两个指针参数都是空（`api` = NULL、`ctx` = NULL——策略 provider
/// 的 per-instance 状态经 `kcomp_service_dispatch` 的 `instance_state` 传递），
/// 不存在指针有效期前提。
pub fn publish_endpoint(port_name: &[u8], port: u32) -> Result<()> {
    // SAFETY: api / ctx 都是空指针；Core 只存不解引用。
    let status = unsafe {
        abi::kcore_endpoint_publish(
            port_name.as_ptr(),
            port_name.len(),
            <SchedulerPolicy as Contract>::ID,
            <SchedulerPolicy as Contract>::KIND.as_u32(),
            <SchedulerPolicy as Contract>::ABI,
            port,
            core::ptr::null(),
            core::ptr::null_mut(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(Errno::from_code(status))
    }
}

// -----------------------------------------------------------------------
// wire 编解码（provider 侧；Core 编码后传入）
// -----------------------------------------------------------------------

/// `CHOOSE_NEXT` 请求的**只读视图**（provider 侧）：Core 编码、provider 解码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChooseNextRequest<'a> {
    current: u32,
    cpu: u32,
    runnable: &'a [u8],
}

impl<'a> ChooseNextRequest<'a> {
    /// 解码 `(args, input)`：结构不符（args 不是 8 字节 / input 空或不是 4 的
    /// 整数倍）→ `None`（provider 应返回 `-EINVAL`）。
    pub fn decode(args: &'a [u8], input: &'a [u8]) -> Option<Self> {
        if args.len() != crate::generated::scheduler::KCOMP_SCHEDULER_ARGS_LEN {
            return None;
        }
        if input.is_empty() || !input.len().is_multiple_of(KCOMP_SCHEDULER_TASK_ID_LEN) {
            return None;
        }
        let current = u32::from_le_bytes([args[0], args[1], args[2], args[3]]);
        Some(Self {
            current,
            cpu: u32::from_le_bytes([args[4], args[5], args[6], args[7]]),
            runnable: input,
        })
    }

    /// 当前 TaskId；`None` = 从锚点（monitor / 组件 init）进入调度。
    pub const fn current(self) -> Option<u32> {
        if self.current == KCOMP_SCHEDULER_NONE {
            None
        } else {
            Some(self.current)
        }
    }

    /// 请求调度的逻辑 CPU；候选列表已由 Core 按此 CPU 的归属裁剪。
    pub const fn cpu(self) -> u32 {
        self.cpu
    }

    /// runnable 列表长度（非空）。
    pub const fn runnable_count(self) -> usize {
        self.runnable.len() / KCOMP_SCHEDULER_TASK_ID_LEN
    }

    /// 读第 `slot` 个 runnable TaskId；越界 → `None`。
    pub fn runnable_at(self, slot: usize) -> Option<u32> {
        let offset = slot.checked_mul(KCOMP_SCHEDULER_TASK_ID_LEN)?;
        let bytes = self
            .runnable
            .get(offset..offset.checked_add(KCOMP_SCHEDULER_TASK_ID_LEN)?)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

/// provider 侧：把提议写进 `output`（必须恰好 `SCHEDULER_TASK_ID_LEN` 字节）。
pub fn write_proposal(output: &mut [u8], task: u32) -> Result<()> {
    if output.len() != KCOMP_SCHEDULER_TASK_ID_LEN {
        return Err(Errno::EINVAL);
    }
    output.copy_from_slice(&task.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合法请求：current / runnable 正确解码。
    #[test]
    fn choose_next_request_decodes_current_and_runnable() {
        let args = [7u32.to_le_bytes(), 3u32.to_le_bytes()].concat();
        let input = [3u32.to_le_bytes(), 5u32.to_le_bytes()].concat();
        let request = ChooseNextRequest::decode(&args, &input).expect("well-formed request");
        assert_eq!(request.current(), Some(7));
        assert_eq!(request.cpu(), 3);
        assert_eq!(request.runnable_count(), 2);
        assert_eq!(request.runnable_at(0), Some(3));
        assert_eq!(request.runnable_at(1), Some(5));
        assert_eq!(request.runnable_at(2), None, "越界必须返回 None");
    }

    /// `NONE` 哨兵 = 无 current（从锚点进入调度）。
    #[test]
    fn choose_next_request_maps_none_sentinel_to_no_current() {
        let args = [SCHEDULER_NONE.to_le_bytes(), 0u32.to_le_bytes()].concat();
        let input = 1u32.to_le_bytes();
        let request = ChooseNextRequest::decode(&args, &input).expect("well-formed request");
        assert_eq!(request.current(), None);
    }

    /// 结构非法的请求一律拒绝（provider 返回 `-EINVAL`）。
    #[test]
    fn choose_next_request_rejects_malformed_frames() {
        let args = 1u32.to_le_bytes();
        assert!(
            ChooseNextRequest::decode(&[], &args).is_none(),
            "args 必须 8 字节"
        );
        assert!(
            ChooseNextRequest::decode(&[0u8; 3], &args).is_none(),
            "args 必须 8 字节"
        );
        assert!(
            ChooseNextRequest::decode(&args, &[]).is_none(),
            "input 必须非空"
        );
        assert!(
            ChooseNextRequest::decode(&args, &[0u8; 5]).is_none(),
            "input 必须是 4 的整数倍"
        );
    }

    /// 提议编码：恰好 4 字节才接受。
    #[test]
    fn write_proposal_requires_exactly_one_task_id() {
        let mut output = [0u8; SCHEDULER_TASK_ID_LEN];
        assert_eq!(write_proposal(&mut output, 9), Ok(()));
        assert_eq!(u32::from_le_bytes(output), 9);

        assert_eq!(write_proposal(&mut [], 9), Err(Errno::EINVAL));
        assert_eq!(write_proposal(&mut [0u8; 5], 9), Err(Errno::EINVAL));
    }

    /// 契约身份锚定（ASCII tag 的大端读数）：schema 数值漂移 = Core 选择直接拒绝。
    #[test]
    fn scheduler_policy_identity_is_anchored() {
        assert_eq!(SCHEDULER_POLICY_NAME, b"scheduler.policy");
        assert_eq!(SCHEDULER_POLICY_ABI.raw(), 0x5343_4845_4443_5055);
        assert_eq!(SchedulerPolicy::ID, 0x5343_4845_4450_4F4C);
        assert_eq!(SchedulerPolicy::ABI, 0x5343_4845_4443_5055);
        assert_eq!(SchedulerPolicy::KIND, InterfaceKind::Policy);
        assert_eq!(SCHEDULER_METHOD_CHOOSE_NEXT, 0);
        assert_eq!(SCHEDULER_TASK_ID_LEN, 4);
        assert_eq!(SCHEDULER_NONE, u32::MAX);
    }
}
