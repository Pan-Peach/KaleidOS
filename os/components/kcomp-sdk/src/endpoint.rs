//! Contract / Endpoint 的 SDK 前端：把 opaque `EndpointId` 收窄成 typed `Endpoint<C>`。
//!
//! # 分工（docs/architecture/deployment.md §1 / §4）
//!
//! - Core owns truth：endpoint 的存在性 / 状态 / owner / contract / abi 全在 Core；
//! - SDK 只实现机制、不选择机制：本模块**不**决定 Direct / Gate——那是 Core 在
//!   bind 时按 `(caller domain, callee domain)` 选定的调用后端；
//!   这里只把 id 变成"带契约身份的句柄"。
//!
//! # 校验只做一次
//!
//! [`Endpoint::from_id`] 调 `kcore_endpoint_validate`（Core 侧
//! `EndpointRegistry::lookup`：contract + abi exact-match + 存活）。校验通过后
//! 身份不再重复校验；每次调用路径仍由 Core 重新做**存活解析**（endpoint 死了
//! 绝不重定向到新实例）。

use core::marker::PhantomData;

use crate::abi;
use crate::errno::{Errno, Result};
use crate::generated::abi::InterfaceKind;

/// 一个组件间契约的**稳定身份**：contract id + exact ABI fingerprint + 领域分类。
///
/// `ID` / `ABI` 必须与 Core 记录的逐位相等，才可能通过
/// `kcore_endpoint_validate`。数值来自 `abi/*.toml` 生成物——不要手写。
pub trait Contract {
    /// 契约身份（组合策略提供的不透明 `u64`）。
    const ID: u64;
    /// exact ABI fingerprint（无版本兼容语义；逐位相等才交付）。
    const ABI: u64;
    /// 领域分类（`Device` / `Service` / `Policy`）。
    const KIND: InterfaceKind;
}
/// Publish only a message endpoint. Core reserves port zero plus null api/ctx
/// for Request/Reply and rejects unsupported execution-domain bindings.
pub fn publish_ipc<C: Contract>(name: &[u8]) -> Result<()> {
    let status = unsafe {
        abi::kcore_endpoint_publish(
            name.as_ptr(),
            name.len(),
            C::ID,
            C::KIND as u32,
            C::ABI,
            0,
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

/// 某契约 `C` 的一个**已校验** endpoint（consumer 侧句柄）。
///
/// 不持裸可调用物（provider 指针不进 consumer）；调用机制由 Core 在 bind 时选定、
/// 由 bind 选定的调用后端承载——本类型只承载**身份** + 契约类型。
pub struct Endpoint<C: Contract> {
    id: u64,
    _c: PhantomData<fn() -> C>,
}

impl<C: Contract> Clone for Endpoint<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C: Contract> Copy for Endpoint<C> {}

impl<C: Contract> PartialEq for Endpoint<C> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<C: Contract> Eq for Endpoint<C> {}

impl<C: Contract> core::fmt::Debug for Endpoint<C> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Endpoint").field("id", &self.id).finish()
    }
}

impl<C: Contract> Endpoint<C> {
    /// 从已持有的 opaque `EndpointId` 构造：调 `kcore_endpoint_validate`
    /// （contract + abi exact-match + 存活）。**唯一的公开安全构造入口**——
    /// 没有 unchecked 构造器。
    pub fn from_id(id: u64) -> Result<Self> {
        // SAFETY: Core 只读校验；三个参数都是值，无指针借用。
        let status = unsafe { abi::kcore_endpoint_validate(id, C::ID, C::ABI) };
        if status == 0 {
            Ok(Self {
                id,
                _c: PhantomData,
            })
        } else {
            Err(Errno::from_code(status))
        }
    }

    /// 组合期发现：`(provider, port_name, contract) → EndpointId`，随后
    /// [`Self::from_id`] 补齐 **abi** 校验（Core 的发现路径只比较 contract + 存活）。
    pub fn lookup(provider: u32, port_name: &[u8]) -> Result<Self> {
        let mut id = 0u64;
        // SAFETY: (port_name_ptr, len) 与 out 在本帧内有效；Core 只读名字并写 out。
        let status = unsafe {
            abi::kcore_endpoint_lookup(
                provider,
                port_name.as_ptr(),
                port_name.len(),
                C::ID,
                &mut id,
            )
        };
        if status != 0 {
            return Err(Errno::from_code(status));
        }
        Self::from_id(id)
    }

    /// opaque EndpointId（诊断 / 传给 C ABI 用；它是身份不是 authority）。
    pub fn id(&self) -> u64 {
        self.id
    }
}

/// 一次 endpoint 调用的失败分类：**传输失败 ≠ 方法失败**。
///
/// `Transport` 表示 provider 从未被调用（Core 拒绝）；`Method` 表示方法层拒绝
/// ——typed 前端按契约直接判定请求无效（`EINVAL`），或 provider 被调用并返回
/// `-errno`；`InvalidReply` 表示 provider 的返回值不符合 `0 / -errno` 契约
/// （无意义的回复）。三者绝不混淆。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvokeError {
    /// Core 传输失败：endpoint 已死 / 调用方无 principal / provider 不 Ready /
    /// image 没有 dispatcher / provider 在边界内 panic 等（见 `kcore_endpoint_call`）。
    Transport(Errno),
    /// 方法失败：请求按契约无效（前端直接挡下，provider 未被调用），或 provider
    /// 被调用并返回 `-errno`。
    Method(Errno),
    /// 传输成功，但 provider 的回复不是 `0 / -errno`（契约违规，不是 UB 兜底）。
    InvalidReply,
}

#[cfg(test)]
mod tests;
