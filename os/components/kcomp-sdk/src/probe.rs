//! `probe.result` 契约 + driver assignment create config —— **无环的 prober → driver
//! 分发**（step 4；见 `abi/probe.toml` 与 `docs/architecture/deployment.md`）。
//!
//! # 为什么需要它
//!
//! 旧流程里 driver 在自己的 create 中回调 prober（`driver.prober` 的
//! `next_assignment` / `report_attempt`），形成
//! `Task(prober) → Driver create → Service(prober)` 同步重入环——endpoint 调用模型
//! 的 re-entry 门禁必须拒绝它。本模块把流程拆成两半，环即消失：
//!
//! ```text
//! prober task
//!   → kcore_component_create(driver, DriverCreateConfig{device_id, 结果端口名})
//!   → create 返回（driver 在自己的 create 身份下 claim + fine match + 发布）
//!   → Endpoint::<ProbeResult>::lookup(instance, 端口名) + pull_result(...)
//!   → prober 用自己的普通函数调用更新 cursor（不是 endpoint 回调）
//! ```
//!
//! # 两个契约面
//!
//! - [`DriverCreateConfig`]：**扁平字节**（无嵌套指针）的 assignment config，
//!   经 `KcompCreateArgs.config` 进入 driver；driver **绝不**回调 prober。
//! - [`ProbeResult`]：driver 在 create 期间 staged publish 的结果端口；
//!   只有 **Gate** transport（结果只在 create 成功后拉取一次，不需要 Direct
//!   function table）。
//!
//! # 错误分类
//!
//! [`pull_result`] 沿用 endpoint 的三分类（传输 / 方法 / 无意义回复），
//! 与 `block` / `filesystem` 的调用后端一致：Core 传输失败与 provider 方法状态
//! 永不混淆。

use crate::abi;
use crate::call;
use crate::endpoint::{Contract, Endpoint, InvokeError};
use crate::errno::{Errno, Result};
use crate::generated::abi::InterfaceKind;

pub use crate::generated::probe::{
    KCOMP_DRIVER_CREATE_CONFIG_ABI, KCOMP_DRIVER_CREATE_DEVICE_ID_OFFSET,
    KCOMP_DRIVER_CREATE_HEADER_LEN, KCOMP_DRIVER_CREATE_NAME_LEN_OFFSET,
    KCOMP_DRIVER_CREATE_NAME_MAX, KCOMP_DRIVER_CREATE_NAME_OFFSET, KCOMP_PROBE_OUTCOME_MATCH,
    KCOMP_PROBE_OUTCOME_NO_MATCH, KCOMP_PROBE_RESULT_ABI, KCOMP_PROBE_RESULT_CONTRACT,
    KCOMP_PROBE_RESULT_METHOD_RESULT, KCOMP_PROBE_RESULT_NAME, KCOMP_PROBE_RESULT_OUTPUT_LEN,
};

// -----------------------------------------------------------------------
// 契约：probe.result（provider: 候选驱动；consumer: driver_prober 自己）
// -----------------------------------------------------------------------

/// `probe.result` 契约（KIND = Service；只有 Gate transport）。
pub struct ProbeResult;

impl Contract for ProbeResult {
    const ID: u64 = KCOMP_PROBE_RESULT_CONTRACT;
    const ABI: u64 = KCOMP_PROBE_RESULT_ABI;
    const KIND: InterfaceKind = InterfaceKind::Service;
}

/// 一次 probe 结果的 8 字节 wire 编码：`outcome` i32 LE + `detail` u32 LE。
///
/// `outcome`：`0` = [`ProbeReply::MATCH`]，`1` = [`ProbeReply::NO_MATCH`]，
/// `< 0` = 创建失败（value = `-Errno`，由 prober 在 create 返回非零时记录——
/// driver 不会发布失败结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeReply {
    /// 见类型文档的 outcome 编码。
    pub outcome: i32,
    /// driver 定义的观测值（例如设备身份读数）；失败时为 0。
    pub detail: u32,
}

impl ProbeReply {
    /// wire 长度（= `KCOMP_PROBE_RESULT_OUTPUT_LEN`）。
    pub const ENCODED_LEN: usize = KCOMP_PROBE_RESULT_OUTPUT_LEN;
    /// outcome 编码：driver 接受该设备（attach + publication 均已成功）。
    pub const MATCH: i32 = KCOMP_PROBE_OUTCOME_MATCH;
    /// outcome 编码：driver 检查后拒绝该设备（无残留 claim、无 block endpoint）。
    pub const NO_MATCH: i32 = KCOMP_PROBE_OUTCOME_NO_MATCH;

    pub const fn new(outcome: i32, detail: u32) -> Self {
        Self { outcome, detail }
    }

    /// 接受该设备（只在 attach + publication 真的成功之后调用）。
    pub const fn matched() -> Self {
        Self::new(Self::MATCH, 0)
    }

    /// 拒绝该设备（`detail` = 观测值，例如读到的协议身份）。
    pub const fn no_match(detail: u32) -> Self {
        Self::new(Self::NO_MATCH, detail)
    }

    /// 创建失败（prober 侧记录用；`error` 是 create 边界返回的 errno）。
    pub const fn creation_failed(error: Errno) -> Self {
        Self::new(error.code(), 0)
    }

    pub const fn is_match(self) -> bool {
        self.outcome == Self::MATCH
    }

    /// 诊断名（日志用；不参与 wire 编码）。
    pub const fn outcome_name(self) -> &'static str {
        match self.outcome {
            Self::MATCH => "Match",
            Self::NO_MATCH => "NoMatch",
            _ => "Error",
        }
    }

    pub const fn encode(self) -> [u8; Self::ENCODED_LEN] {
        let outcome = self.outcome.to_le_bytes();
        let detail = self.detail.to_le_bytes();
        [
            outcome[0], outcome[1], outcome[2], outcome[3], detail[0], detail[1], detail[2],
            detail[3],
        ]
    }

    pub const fn decode(bytes: [u8; Self::ENCODED_LEN]) -> Self {
        Self {
            outcome: i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            detail: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }
}

// -----------------------------------------------------------------------
// DriverCreateConfig：assignment 数据的扁平编码
// -----------------------------------------------------------------------

/// driver 的 assignment create config：**扁平字节，无嵌套指针**。
///
/// 布局（`abi/probe.toml` 单一来源）：
///
/// ```text
/// offset 0 : device_id         u32 LE
/// offset 4 : endpoint_name_len u32 LE
/// offset 8 : endpoint_name     bytes（长度 = endpoint_name_len，无 NUL）
/// ```
///
/// 校验（[`Self::encode`] / [`Self::decode`] 同一套，两侧都做）：
/// `config_abi` 由调用方核对；`1 <= N <= KCOMP_DRIVER_CREATE_NAME_MAX`；总长恰好
/// `8 + N`；端口名不得等于保留名 [`KCOMP_PROBE_RESULT_NAME`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverCreateConfig<'a> {
    /// prober 分配的候选设备（**选择数据，不是权限**）；driver 自己 claim。
    pub device_id: u32,
    /// driver 的 `probe.result` 结果端口名（prober 按 attempt 生成，实例内唯一）。
    pub endpoint_name: &'a [u8],
}

impl<'a> DriverCreateConfig<'a> {
    /// 编码后的最大长度（固定头部 + 名字上限）。
    pub const MAX_ENCODED_LEN: usize =
        KCOMP_DRIVER_CREATE_HEADER_LEN + KCOMP_DRIVER_CREATE_NAME_MAX;

    pub const fn new(device_id: u32, endpoint_name: &'a [u8]) -> Self {
        Self {
            device_id,
            endpoint_name,
        }
    }

    /// 写进 `out`（调用方保证 `out.len() >= Self::MAX_ENCODED_LEN`），返回总长。
    ///
    /// 校验失败 / 缓冲区不够 → `Err(EINVAL)`，`out` 内容不保证（调用方不应读）。
    pub fn encode(self, out: &mut [u8]) -> Result<usize> {
        checked_name(self.endpoint_name)?;
        let name_len = self.endpoint_name.len();
        let total = KCOMP_DRIVER_CREATE_HEADER_LEN + name_len;
        if out.len() < total {
            return Err(Errno::EINVAL);
        }
        out[..4].copy_from_slice(&self.device_id.to_le_bytes());
        out[4..8].copy_from_slice(&(name_len as u32).to_le_bytes());
        out[KCOMP_DRIVER_CREATE_NAME_OFFSET..total].copy_from_slice(self.endpoint_name);
        Ok(total)
    }

    /// 解析一段完整 config 字节（总长必须恰好 `8 + N`）。
    pub fn decode(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() < KCOMP_DRIVER_CREATE_HEADER_LEN {
            return Err(Errno::EINVAL);
        }
        let device_id = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let name_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let name_len = checked_len(name_len)?;
        if bytes.len() != KCOMP_DRIVER_CREATE_HEADER_LEN + name_len {
            return Err(Errno::EINVAL);
        }
        let endpoint_name = &bytes[KCOMP_DRIVER_CREATE_NAME_OFFSET..];
        checked_name(endpoint_name)?;
        Ok(Self {
            device_id,
            endpoint_name,
        })
    }

    /// 从 `kcomp_instance_create` 的 args 解析 assignment（driver 侧入口）。
    ///
    /// # Safety
    ///
    /// `args` 必须指向一个在本次 create 调用期间有效的 `KcompCreateArgs`
    /// （Core 的 create ABI 契约）；`args.config` 的 `(ptr, len)` 必须描述同一
    /// 有效区。本函数先核对 `config_abi` 与长度上界，再构造切片（绝不会用未校验的
    /// `config_len` 构造越界切片）。
    pub unsafe fn from_create_args(args: &'a abi::KcompCreateArgs) -> Result<Self> {
        if args.config_abi != KCOMP_DRIVER_CREATE_CONFIG_ABI {
            return Err(Errno::EINVAL);
        }
        if args.config.is_null()
            || args.config_len < KCOMP_DRIVER_CREATE_HEADER_LEN
            || args.config_len > Self::MAX_ENCODED_LEN
        {
            return Err(Errno::EINVAL);
        }
        // SAFETY: 调用方保证 (config, config_len) 有效；长度已收敛到
        // `[HEADER_LEN, MAX_ENCODED_LEN]`，切片不会越出调用方声明的缓冲区。
        let bytes =
            unsafe { core::slice::from_raw_parts(args.config.cast::<u8>(), args.config_len) };
        Self::decode(bytes)
    }
}

/// `endpoint_name` 的长度与保留名校验（编码 / 解码共用的唯一入口）。
fn checked_name(name: &[u8]) -> Result<()> {
    if name.is_empty() || name.len() > KCOMP_DRIVER_CREATE_NAME_MAX {
        return Err(Errno::EINVAL);
    }
    if name == KCOMP_PROBE_RESULT_NAME {
        // 保留名不得被动态结果端口占用（prober 生成的 `probe.result.<attempt>` 天然不同）。
        return Err(Errno::EINVAL);
    }
    Ok(())
}

fn checked_len(name_len: u32) -> Result<usize> {
    let len = name_len as usize;
    if len == 0 || len > KCOMP_DRIVER_CREATE_NAME_MAX {
        return Err(Errno::EINVAL);
    }
    Ok(len)
}

// -----------------------------------------------------------------------
// prober 侧：结果端口名 + pull
// -----------------------------------------------------------------------

/// `probe.result.<attempt>` 名字缓冲上限（前缀 13 字节 + `u32::MAX` 十进制 10 位 = 23）。
pub const RESULT_PORT_NAME_MAX: usize = 32;

/// prober 为 attempt 生成结果端口名：`probe.result.<十进制 attempt>`（无 NUL）。
///
/// `attempt == 0`（cursor 保留的空值）→ `Err(EINVAL)`；返回写入 `out` 的长度。
pub fn result_port_name(attempt: u32, out: &mut [u8]) -> Result<usize> {
    const PREFIX: &[u8] = b"probe.result.";
    if attempt == 0 {
        return Err(Errno::EINVAL);
    }
    let mut digits = [0u8; 10];
    let mut count = 0;
    let mut value = attempt;
    while value > 0 {
        digits[count] = b'0' + (value % 10) as u8;
        value /= 10;
        count += 1;
    }
    let total = PREFIX.len() + count;
    if out.len() < total {
        return Err(Errno::EINVAL);
    }
    out[..PREFIX.len()].copy_from_slice(PREFIX);
    for index in 0..count {
        out[PREFIX.len() + index] = digits[count - 1 - index];
    }
    Ok(total)
}

/// provider 侧：发布本实例的 `probe.result` endpoint（**Gate transport**）。
///
/// 结果只在 create 返回 0 之后被拉取一次，因此没有 Direct function table：
/// `api` 永远为空。发布本身是 staged 的（`kcomp_instance_create` 期间只记录
/// pending；create 返回 0 后 Core 原子提交）。
///
/// # Safety
///
/// `ctx` 必须是本 provider 实例存活期内地址稳定的 opaque state——Core 只存指针、
/// 不解引用；Gate 分派会把它原样交给 `kcomp_service_dispatch`。
pub unsafe fn publish_result_endpoint(port_name: &[u8], port: u32, ctx: *mut ()) -> Result<()> {
    // SAFETY: 调用方保证 ctx 的实例生命周期（见 Safety）；`api` 为 null 表示本
    // 契约不提供 Direct transport（bind 选 Direct 时 Core 以 ENOTSUP 显式拒绝）。
    let status = unsafe {
        abi::kcore_endpoint_publish(
            port_name.as_ptr(),
            port_name.len(),
            <ProbeResult as Contract>::ID,
            <ProbeResult as Contract>::KIND.as_u32(),
            <ProbeResult as Contract>::ABI,
            port,
            core::ptr::null(),
            ctx,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(Errno::from_code(status))
    }
}

/// consumer 侧（prober）：拉取一次结果（`RESULT` 方法；args / input 空）。
pub fn pull_result(
    endpoint: Endpoint<ProbeResult>,
) -> core::result::Result<ProbeReply, InvokeError> {
    let mut reply = [0u8; ProbeReply::ENCODED_LEN];
    match call::endpoint_call(
        endpoint.id(),
        KCOMP_PROBE_RESULT_METHOD_RESULT,
        &[],
        &[],
        &mut reply,
    ) {
        Err(errno) => Err(InvokeError::Transport(errno)),
        Ok(0) => Ok(ProbeReply::decode(reply)),
        Ok(status) if status < 0 => Err(InvokeError::Method(Errno::from_code(status))),
        Ok(_) => Err(InvokeError::InvalidReply),
    }
}

#[cfg(test)]
mod tests;
