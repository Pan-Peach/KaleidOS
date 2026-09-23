//! `block.device` 的**调用后端**：Core 在 bind 时选定的机制（Direct / Gate），
//! SDK 只**实现**、不**选择**。
//!
//! - **Direct**（同域 KernelNative）：直接调 provider 的 `#[repr(C)]` function
//!   table（`api` + `ctx`）——稳态零 Core 介入、零分配、零消息打包；
//! - **Gate**（跨域 / 需 containment）：经 `kcore_endpoint_call` 的 Core call gate，
//!   线格式与 Direct 完全一致（[`crate::block::dispatch`] 的扁平编码）。
//!
//! 两条路都在本模块收口，consumer 只看到 [`crate::block::client::BlockBinding`]
//! 的 typed 方法——调用点与机制无关（`block.read(lba, &mut buf)`）。
//!
//! # 为什么 `api` / `ctx` 是 `unsafe` 的边界
//!
//! Core 在 bind 时已经校验 exact contract + abi + 存活，并保证 Direct 回复携带
//! 非空 function table；本模块的 `unsafe` 只表达"解引用 Core 交付的 provider
//! 指针"这一件事（Core 自己不解引用）。

use crate::abi;
use crate::block::dispatch::encode_lba;
use crate::block::{BlockDevice, BlockDeviceApi};
use crate::call;
use crate::endpoint::{Contract, InvokeError};
use crate::errno::Errno;
use crate::generated::block::{
    KCOMP_BLOCK_CAPACITY_LEN, KCOMP_BLOCK_METHOD_CAPACITY, KCOMP_BLOCK_METHOD_READ,
    KCOMP_BLOCK_METHOD_WRITE,
};

/// 机制专有信息（**SDK 私有**）：消费者拿不到、也构造不出裸 function table。
pub(super) enum Backend {
    /// 同域 Direct：provider 的 function table + opaque state（Core bind 交付）。
    Direct {
        api: *const BlockDeviceApi,
        ctx: *mut (),
    },
    /// 跨域 / 需 containment：opaque EndpointId（Core call-gate handle）。
    Gate { endpoint: u64 },
}

/// 调 `kcore_endpoint_bind`：Core 校验（exact contract + abi + 存活）并**一次性**
/// 选定机制。DIRECT 回复必须带非空 function table；GATE 回复只给 opaque id
/// （`api` / `ctx` 不被写）。
pub(super) fn bind(endpoint: u64) -> Result<Backend, InvokeError> {
    let (mut mechanism, mut api, mut ctx) = (0u32, 0usize, 0usize);
    // SAFETY: 三个 out 都在本帧内有效；Core 只写 out，不解引用任何东西。
    let status = unsafe {
        abi::kcore_endpoint_bind(
            endpoint,
            <BlockDevice as Contract>::ID,
            <BlockDevice as Contract>::ABI,
            &mut mechanism,
            &mut api,
            &mut ctx,
        )
    };
    if status != 0 {
        return Err(InvokeError::Transport(Errno::from_code(status)));
    }
    match mechanism {
        abi::KCORE_ENDPOINT_MECHANISM_DIRECT => {
            if api == 0 {
                // Core 契约：Direct 必带非空 function table。空 = 回复违反契约，
                // 绝不把 null table 当成可调用物（更不降级）。
                return Err(InvokeError::InvalidReply);
            }
            Ok(Backend::Direct {
                api: api as *const BlockDeviceApi,
                ctx: ctx as *mut (),
            })
        }
        abi::KCORE_ENDPOINT_MECHANISM_GATE => Ok(Backend::Gate { endpoint }),
        // 未知机制编码 = Core 回复违反契约（不猜测、不降级成 Direct）。
        _ => Err(InvokeError::InvalidReply),
    }
}

/// 设备容量（sector 数）。
pub(super) fn capacity_sectors(backend: &Backend) -> Result<u64, InvokeError> {
    match backend {
        Backend::Direct { api, ctx } => {
            let table = table(*api);
            // SAFETY: Core bind 保证 Direct 的 `api` 非空且指向 provider 的
            // 'static #[repr(C)] BlockDeviceApi（exact ABI 已校验）；`ctx` 原样回传。
            Ok(unsafe { (table.capacity_sectors)(*ctx) })
        }
        Backend::Gate { endpoint } => {
            let mut reply = [0u8; KCOMP_BLOCK_CAPACITY_LEN];
            invoke_gate(*endpoint, KCOMP_BLOCK_METHOD_CAPACITY, &[], &[], &mut reply)?;
            Ok(u64::from_le_bytes(reply))
        }
    }
}

/// 从 `lba` 读 `buf.len()` 字节到 `buf`（长度合法性由 typed 前端先行校验）。
pub(super) fn read(backend: &Backend, lba: u64, buf: &mut [u8]) -> Result<(), InvokeError> {
    match backend {
        Backend::Direct { api, ctx } => {
            let table = table(*api);
            // SAFETY: 同 capacity_sectors；`buf` 是本帧内的有效可写切片，契约要求
            // 它位于 Core 可见 RAM（v1 无 IOMMU：设备地址 == 物理地址 == 虚拟地址）。
            map_status(unsafe { (table.read)(*ctx, lba, buf.as_mut_ptr(), buf.len()) })
        }
        Backend::Gate { endpoint } => invoke_gate(
            *endpoint,
            KCOMP_BLOCK_METHOD_READ,
            &encode_lba(lba),
            &[],
            buf,
        ),
    }
}

/// 从 `buf` 写 `buf.len()` 字节到 `lba`（长度合法性由 typed 前端先行校验）。
pub(super) fn write(backend: &Backend, lba: u64, buf: &[u8]) -> Result<(), InvokeError> {
    match backend {
        Backend::Direct { api, ctx } => {
            let table = table(*api);
            // SAFETY: 同 capacity_sectors；`buf` 是本帧内的有效只读切片。
            map_status(unsafe { (table.write)(*ctx, lba, buf.as_ptr(), buf.len()) })
        }
        Backend::Gate { endpoint } => invoke_gate(
            *endpoint,
            KCOMP_BLOCK_METHOD_WRITE,
            &encode_lba(lba),
            buf,
            &mut [],
        ),
    }
}

/// Core bind 交付的 function table：`'static`（provider image pinned-until-reboot）。
///
/// # Safety
/// `api` 必须来自 Core 的 DIRECT bind 回复（非空 + exact ABI 已校验）。
fn table(api: *const BlockDeviceApi) -> &'static BlockDeviceApi {
    // SAFETY: 调用方（本模块）只从 `bind` 的 DIRECT 分支构造 `Backend::Direct`；
    // 该分支已拒绝 null，Core 也已校验 provider Ready + exact ABI。
    unsafe { &*api }
}

/// provider 的 `0 / -errno` → [`InvokeError`]。
///
/// Direct 没有传输层：非零**正数**不是 Core 状态，而是 provider 违反
/// `0 / -errno` 契约 → [`InvokeError::InvalidReply`]。
fn map_status(status: i32) -> Result<(), InvokeError> {
    match status {
        0 => Ok(()),
        status if status < 0 => Err(InvokeError::Method(Errno::from_code(status))),
        _ => Err(InvokeError::InvalidReply),
    }
}

/// Gate 路径：`kcore_endpoint_call` 的传输状态与 provider 方法状态分离翻译
/// （与 Direct 分支的 [`map_status`] 同一分类）。
fn invoke_gate(
    endpoint: u64,
    method: u32,
    args: &[u8],
    input: &[u8],
    output: &mut [u8],
) -> Result<(), InvokeError> {
    match call::endpoint_call(endpoint, method, args, input, output) {
        Err(errno) => Err(InvokeError::Transport(errno)),
        Ok(0) => Ok(()),
        Ok(status) if status < 0 => Err(InvokeError::Method(Errno::from_code(status))),
        Ok(_) => Err(InvokeError::InvalidReply),
    }
}
