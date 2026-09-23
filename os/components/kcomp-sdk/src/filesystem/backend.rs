//! `filesystem` 的**调用后端**：Core 在 bind 时选定的机制（Direct / Gate），
//! SDK 只**实现**、不**选择**。
//!
//! - **Direct**（同域 KernelNative）：直接调 provider 的 `#[repr(C)]` function
//!   table（`api` + `ctx`）——稳态零 Core 介入、零分配、零打包；
//! - **Gate**（跨域 / 需 containment）：经 `kcore_endpoint_call` 的 Core call gate，
//!   线格式与 Direct 完全一致（[`crate::filesystem::dispatch`] 的扁平编码）。
//!
//! 两条路都在本模块收口，consumer 只看到 [`crate::filesystem::client::FileSystemBinding`]
//! 的 typed 方法——调用点与机制无关（`fs.open(path, flags)`）。
//!
//! # `read` 的缓冲区布局（两条机制一致）
//!
//! typed 前端的 `buf` **就是**扁平 frame 的 `output` 区：前
//! [`KCOMP_FILESYSTEM_READ_HEADER_LEN`] 字节是 LE `u64` 实际长度头，数据从 offset 8
//! 开始；`buf.len()` 含头（数据容量 = `buf.len() - 8`）。Direct 分支因此也把 provider
//! 写出的数据留在 offset 8 并回填同一份头——两条机制对 consumer 呈现的缓冲区字节
//! **完全一致**，consumer 不做机制分支。
//!
//! # 为什么 `api` / `ctx` 是 `unsafe` 的边界
//!
//! Core 在 bind 时已经校验 exact contract + abi + 存活，并保证 Direct 回复携带
//! 非空 function table；本模块的 `unsafe` 只表达"解引用 Core 交付的 provider
//! 指针"这一件事（Core 自己不解引用）。

use core::ffi::CStr;

use crate::abi;
use crate::call;
use crate::endpoint::{Contract, InvokeError};
use crate::errno::Errno;
use crate::filesystem::dispatch::{
    decode_handle, decode_read_len, encode_flags, encode_handle, encode_read_len,
};
use crate::filesystem::{FileSystem, FileSystemApi};
use crate::generated::filesystem::{
    KCOMP_FILESYSTEM_HANDLE_LEN, KCOMP_FILESYSTEM_METHOD_CLOSE, KCOMP_FILESYSTEM_METHOD_MOUNT,
    KCOMP_FILESYSTEM_METHOD_OPEN, KCOMP_FILESYSTEM_METHOD_READ, KCOMP_FILESYSTEM_METHOD_UNMOUNT,
    KCOMP_FILESYSTEM_READ_HEADER_LEN,
};

/// 机制专有信息（**SDK 私有**）：消费者拿不到、也构造不出裸 function table。
pub(super) enum Backend {
    /// 同域 Direct：provider 的 function table + opaque state（Core bind 交付）。
    Direct {
        api: *const FileSystemApi,
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
            <FileSystem as Contract>::ID,
            <FileSystem as Contract>::ABI,
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
                api: api as *const FileSystemApi,
                ctx: ctx as *mut (),
            })
        }
        abi::KCORE_ENDPOINT_MECHANISM_GATE => Ok(Backend::Gate { endpoint }),
        // 未知机制编码 = Core 回复违反契约（不猜测、不降级成 Direct）。
        _ => Err(InvokeError::InvalidReply),
    }
}

/// 挂载（`mount` / `unmount` 无参数、无负载、无回复）。
pub(super) fn mount(backend: &Backend) -> Result<(), InvokeError> {
    match backend {
        Backend::Direct { api, ctx } => {
            let table = table(*api);
            // SAFETY: Core bind 保证 Direct 的 `api` 非空且指向 provider 的
            // 'static #[repr(C)] FileSystemApi（exact ABI 已校验）；`ctx` 原样回传。
            map_status(unsafe { (table.mount)(*ctx) })
        }
        Backend::Gate { endpoint } => {
            invoke_gate(*endpoint, KCOMP_FILESYSTEM_METHOD_MOUNT, &[], &[], &mut [])
        }
    }
}

pub(super) fn unmount(backend: &Backend) -> Result<(), InvokeError> {
    match backend {
        Backend::Direct { api, ctx } => {
            let table = table(*api);
            // SAFETY: 同 mount。
            map_status(unsafe { (table.unmount)(*ctx) })
        }
        Backend::Gate { endpoint } => invoke_gate(
            *endpoint,
            KCOMP_FILESYSTEM_METHOD_UNMOUNT,
            &[],
            &[],
            &mut [],
        ),
    }
}

/// 打开 `path`，返回 provider 的不透明 handle。
pub(super) fn open(backend: &Backend, path: &CStr, flags: u32) -> Result<u64, InvokeError> {
    match backend {
        Backend::Direct { api, ctx } => {
            let table = table(*api);
            let mut handle = 0u64;
            // SAFETY: 同 mount；`path` 是 NUL 结尾的 C 字符串（`&CStr`），
            // `&mut handle` 指向本帧栈变量，调用期间有效。
            map_status(unsafe { (table.open)(*ctx, path.as_ptr().cast(), flags, &mut handle) })?;
            Ok(handle)
        }
        Backend::Gate { endpoint } => {
            let mut reply = [0u8; KCOMP_FILESYSTEM_HANDLE_LEN];
            invoke_gate(
                *endpoint,
                KCOMP_FILESYSTEM_METHOD_OPEN,
                &encode_flags(flags),
                path.to_bytes_with_nul(),
                &mut reply,
            )?;
            decode_handle(&reply).ok_or(InvokeError::InvalidReply)
        }
    }
}

/// 关闭一个 [`open`] 返回的 handle。
pub(super) fn close(backend: &Backend, handle: u64) -> Result<(), InvokeError> {
    match backend {
        Backend::Direct { api, ctx } => {
            let table = table(*api);
            // SAFETY: 同 mount。
            map_status(unsafe { (table.close)(*ctx, handle) })
        }
        Backend::Gate { endpoint } => invoke_gate(
            *endpoint,
            KCOMP_FILESYSTEM_METHOD_CLOSE,
            &encode_handle(handle),
            &[],
            &mut [],
        ),
    }
}

/// 从 `handle` 当前位置读 `buf.len() - 8` 字节到 `buf` 的数据区；返回实际读取长度。
///
/// `buf` 的布局见模块文档（前 8 字节是长度头）；typed 前端保证 `buf.len() >= 8`。
pub(super) fn read(backend: &Backend, handle: u64, buf: &mut [u8]) -> Result<usize, InvokeError> {
    match backend {
        Backend::Direct { api, ctx } => {
            let table = table(*api);
            let (header, data) = buf.split_at_mut(KCOMP_FILESYSTEM_READ_HEADER_LEN);
            let mut actual = 0usize;
            // SAFETY: 同 mount；`data` 是本帧内的有效可写切片，契约要求它位于 Core
            // 可见 RAM（v1 无 IOMMU：设备地址 == 物理地址 == 虚拟地址）。
            let status =
                unsafe { (table.read)(*ctx, handle, data.as_mut_ptr(), data.len(), &mut actual) };
            map_status(status)?;
            if actual > data.len() {
                // provider 返回超过 buffer 的长度 = 契约违约（不截断、不 UB 兜底）。
                return Err(InvokeError::InvalidReply);
            }
            header.copy_from_slice(&encode_read_len(actual));
            Ok(actual)
        }
        Backend::Gate { endpoint } => {
            invoke_gate(
                *endpoint,
                KCOMP_FILESYSTEM_METHOD_READ,
                &encode_handle(handle),
                &[],
                buf,
            )?;
            let actual = decode_read_len(&buf[..KCOMP_FILESYSTEM_READ_HEADER_LEN])
                .ok_or(InvokeError::InvalidReply)?;
            if actual > buf.len() - KCOMP_FILESYSTEM_READ_HEADER_LEN {
                return Err(InvokeError::InvalidReply);
            }
            Ok(actual)
        }
    }
}

/// Core bind 交付的 function table：`'static`（provider image pinned-until-reboot）。
///
/// # Safety
/// `api` 必须来自 Core 的 DIRECT bind 回复（非空 + exact ABI 已校验）。
fn table(api: *const FileSystemApi) -> &'static FileSystemApi {
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
