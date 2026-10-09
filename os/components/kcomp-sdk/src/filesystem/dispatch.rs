//! `filesystem` 的 **Gate 侧适配器**：把扁平 frame 的 method 分派到业务后端。
//!
//! 这里只有**一个** `match method`（方法号来自 `abi/filesystem.toml` 生成物），
//! 严格校验 frame 形状（args 长度 / 路径有界且 NUL 结尾 / output 大小），畸形帧返回
//! `-EINVAL` 且 provider **不被调用**。业务后端是 [`FileSystemProvider`]——同一份
//! 实现同时服务 Direct（[`crate::filesystem::FileSystemService`] 的 function table）
//! 与 Gate（本模块）；业务代码不感知部署。
//!
//! # 线格式（schema 单一来源：`abi/filesystem.toml`）
//!
//! ```text
//! mount(0)  ：args 空、input 空、output 空
//! unmount(1)：args 空、input 空、output 空
//! open(2)   ：args 恰好 4 字节 LE u32 flags、input = NUL 结尾路径（含结尾 NUL、
//!             1..=PATH_MAX、结尾 NUL 是唯一 NUL）、output 恰好 8 字节 LE u64 handle
//! close(3)  ：args 恰好 8 字节 LE u64 handle、input 空、output 空
//! read(4)   ：args 恰好 8 字节 LE u64 handle、input 空、output >= 8 字节：
//!             前 8 字节 = LE u64 实际长度（仅方法返回 0 时有效），其后是数据区
//! ```
//!
//! C 提供方（fatfs）手写的 `kcomp_service_dispatch` 与这些校验逐条同构：Rust 侧用
//! `CStr::from_bytes_with_nul` 拒绝内部 NUL，C 侧显式扫描；两侧共用同一份生成常量。

use core::ffi::CStr;

use crate::errno::Errno;
use crate::filesystem::FileSystemProvider;
use crate::frame::Call;
use crate::generated::filesystem::{
    KCOMP_FILESYSTEM_FLAGS_LEN, KCOMP_FILESYSTEM_HANDLE_LEN, KCOMP_FILESYSTEM_LOOKUP_ARGS_LEN,
    KCOMP_FILESYSTEM_METHOD_CLOSE, KCOMP_FILESYSTEM_METHOD_LOOKUP, KCOMP_FILESYSTEM_METHOD_MOUNT,
    KCOMP_FILESYSTEM_METHOD_NODE_INFO, KCOMP_FILESYSTEM_METHOD_OPEN, KCOMP_FILESYSTEM_METHOD_READ,
    KCOMP_FILESYSTEM_METHOD_ROOT, KCOMP_FILESYSTEM_METHOD_UNMOUNT, KCOMP_FILESYSTEM_NAME_MAX,
    KCOMP_FILESYSTEM_PATH_MAX, KCOMP_FILESYSTEM_READ_HEADER_LEN,
};

/// 分派一次 `filesystem` 调用：`port` 已由 image 级 switch
/// （[`crate::kcomp_services!`]）选中本契约，`method` 在本函数里落到具体后端方法。
///
/// 返回 `0 / -errno`（provider 语义；Core 的传输状态在调用方单独编码）。
pub fn dispatch<P: FileSystemProvider>(p: &P, method: u32, call: Call<'_>) -> i32 {
    match method {
        KCOMP_FILESYSTEM_METHOD_MOUNT => mount(p, call),
        KCOMP_FILESYSTEM_METHOD_UNMOUNT => unmount(p, call),
        KCOMP_FILESYSTEM_METHOD_OPEN => open(p, call),
        KCOMP_FILESYSTEM_METHOD_CLOSE => close(p, call),
        KCOMP_FILESYSTEM_METHOD_READ => read(p, call),
        KCOMP_FILESYSTEM_METHOD_ROOT => root(p, call),
        KCOMP_FILESYSTEM_METHOD_LOOKUP => lookup(p, call),
        KCOMP_FILESYSTEM_METHOD_NODE_INFO => node_info(p, call),
        // 能力缺失（不是畸形帧）：与 Core 对"没有 dispatcher"的档位一致。
        _ => Errno::ENOSYS.code(),
    }
}

/// `Ok(())` → `0`；`Err(e)` → `e.code()`。
fn map(result: crate::errno::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

/// 无参方法的 frame 形状：args / input / output 都必须为空。
fn empty_frame(call: &Call<'_>) -> bool {
    call.args.is_empty() && call.input.is_empty() && call.output.is_empty()
}

fn mount<P: FileSystemProvider>(p: &P, call: Call<'_>) -> i32 {
    if !empty_frame(&call) {
        return Errno::EINVAL.code();
    }
    map(p.mount())
}

fn unmount<P: FileSystemProvider>(p: &P, call: Call<'_>) -> i32 {
    if !empty_frame(&call) {
        return Errno::EINVAL.code();
    }
    map(p.unmount())
}

fn open<P: FileSystemProvider>(p: &P, call: Call<'_>) -> i32 {
    let Some(flags) = decode_flags(call.args) else {
        return Errno::EINVAL.code();
    };
    if call.output.len() != KCOMP_FILESYSTEM_HANDLE_LEN {
        return Errno::EINVAL.code();
    }
    // 有界 + 结尾 NUL 是唯一 NUL：`from_bytes_with_nul` 同时拒绝空路径 / 缺结尾
    // NUL / 内部 NUL；长度上限是契约常量。
    let Ok(path) = CStr::from_bytes_with_nul(call.input) else {
        return Errno::EINVAL.code();
    };
    if call.input.len() > KCOMP_FILESYSTEM_PATH_MAX {
        return Errno::EINVAL.code();
    }
    match p.open(path, flags) {
        Ok(handle) => {
            call.output.copy_from_slice(&encode_handle(handle));
            0
        }
        Err(error) => error.code(),
    }
}

fn close<P: FileSystemProvider>(p: &P, call: Call<'_>) -> i32 {
    let Some(handle) = decode_handle(call.args) else {
        return Errno::EINVAL.code();
    };
    if !call.input.is_empty() || !call.output.is_empty() {
        return Errno::EINVAL.code();
    }
    map(p.close(handle))
}

fn read<P: FileSystemProvider>(p: &P, call: Call<'_>) -> i32 {
    let Some(handle) = decode_handle(call.args) else {
        return Errno::EINVAL.code();
    };
    if !call.input.is_empty() || !is_read_output_len(call.output.len()) {
        return Errno::EINVAL.code();
    }
    let (header, data) = call.output.split_at_mut(KCOMP_FILESYSTEM_READ_HEADER_LEN);
    match p.read(handle, data) {
        // provider 返回超过数据容量的长度 = 契约违约（不是 UB 兜底）。
        Ok(actual) if actual <= data.len() => {
            header.copy_from_slice(&encode_read_len(actual));
            0
        }
        Ok(_) => Errno::EIO.code(),
        Err(error) => error.code(),
    }
}

fn root<P: FileSystemProvider>(p: &P, call: Call<'_>) -> i32 {
    if !call.args.is_empty() || !call.input.is_empty() || call.output.len() != 8 {
        return Errno::EINVAL.code();
    }
    match p.root() {
        Ok(node) if node != 0 => {
            call.output.copy_from_slice(&node.to_le_bytes());
            0
        }
        Ok(_) => Errno::EIO.code(),
        Err(error) => error.code(),
    }
}

fn lookup<P: FileSystemProvider>(p: &P, call: Call<'_>) -> i32 {
    if call.args.len() != KCOMP_FILESYSTEM_LOOKUP_ARGS_LEN
        || call.input.is_empty()
        || call.input.len() > KCOMP_FILESYSTEM_NAME_MAX
        || call.output.len() != 8
    {
        return Errno::EINVAL.code();
    }
    let parent = decode_handle(&call.args[..8]).expect("checked args length");
    let encoding = decode_flags(&call.args[8..]).expect("checked args length");
    match p.lookup(parent, call.input, encoding) {
        Ok(node) if node != 0 => {
            call.output.copy_from_slice(&node.to_le_bytes());
            0
        }
        Ok(_) => Errno::EIO.code(),
        Err(error) => error.code(),
    }
}

fn node_info<P: FileSystemProvider>(p: &P, call: Call<'_>) -> i32 {
    let Some(node) = decode_handle(call.args) else {
        return Errno::EINVAL.code();
    };
    if !call.input.is_empty() || call.output.len() != 4 {
        return Errno::EINVAL.code();
    }
    match p.node_info(node) {
        Ok(kind) => {
            call.output.copy_from_slice(&kind.to_le_bytes());
            0
        }
        Err(error) => error.code(),
    }
}

/// `flags` 的 `args` 编码：一个 LE `u32`（与 C 包装逐字节一致，见
/// `kcomp_filesystem.h`）。
pub(super) fn encode_flags(flags: u32) -> [u8; KCOMP_FILESYSTEM_FLAGS_LEN] {
    flags.to_le_bytes()
}

/// `args` 解码：长度必须恰为 [`KCOMP_FILESYSTEM_FLAGS_LEN`]。
pub(super) fn decode_flags(args: &[u8]) -> Option<u32> {
    let bytes: [u8; KCOMP_FILESYSTEM_FLAGS_LEN] = args.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// `handle` 的 `args` / `output` 编码：一个 LE `u64`。
pub(super) fn encode_handle(handle: u64) -> [u8; KCOMP_FILESYSTEM_HANDLE_LEN] {
    handle.to_le_bytes()
}

/// `args` 解码：长度必须恰为 [`KCOMP_FILESYSTEM_HANDLE_LEN`]。
pub(super) fn decode_handle(args: &[u8]) -> Option<u64> {
    let bytes: [u8; KCOMP_FILESYSTEM_HANDLE_LEN] = args.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

/// `read` 的 `output` 头编码：一个 LE `u64` 实际读取长度。
pub(super) fn encode_read_len(actual: usize) -> [u8; KCOMP_FILESYSTEM_READ_HEADER_LEN] {
    (actual as u64).to_le_bytes()
}

/// `read` 的 `output` 头解码：长度必须恰为
/// [`KCOMP_FILESYSTEM_READ_HEADER_LEN`] 且放得进 `usize`（RV32 溢出 → `None`）。
pub(super) fn decode_read_len(header: &[u8]) -> Option<usize> {
    let bytes: [u8; KCOMP_FILESYSTEM_READ_HEADER_LEN] = header.try_into().ok()?;
    usize::try_from(u64::from_le_bytes(bytes)).ok()
}

/// `read` 的 output 长度合法性：至少放得下 8 字节头（数据容量可以为 0）。
///
/// typed 前端（[`crate::filesystem::client`]）与 provider 侧 dispatch 共用同一条
/// 规则——消费侧早拒与部署无关（Direct / Gate 行为一致）。
pub(super) fn is_read_output_len(len: usize) -> bool {
    len >= KCOMP_FILESYSTEM_READ_HEADER_LEN
}

#[cfg(test)]
mod behaviour_tests;
#[cfg(test)]
mod node_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wire_tests;
