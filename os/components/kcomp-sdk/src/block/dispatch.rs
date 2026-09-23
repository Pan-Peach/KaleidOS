//! `block.device` 的 **Gate 侧适配器**：把扁平 frame 的 method 分派到业务后端。
//!
//! 这里只有**一个** `match method`（方法号来自 `abi/block.toml` 生成物），严格校验
//! frame 形状（args 长度 / 负载长度 / 512 整数倍 / 空区要求），畸形帧返回
//! `-EINVAL` 且 provider **不被调用**。业务后端是 [`BlockDeviceProvider`]——同一份
//! 实现同时服务 Direct（`BlockDeviceService` 的 function table adapter）与
//! Gate（本模块）；业务代码不感知部署。
//!
//! # 线格式（schema 单一来源：`abi/block.toml`）
//!
//! ```text
//! capacity(0)：args 空、input 空、output 恰好 8 字节 LE u64
//! read(1)    ：args 恰好 8 字节 LE u64 lba、input 空、output 非零且 512 整数倍
//! write(2)   ：args 恰好 8 字节 LE u64 lba、input 非零且 512 整数倍、output 空
//! ```
//!
//! 没有单独编码的长度：`input_len` / `output_len` **就是**传输长度。

use crate::block::BlockDeviceProvider;
use crate::errno::Errno;
use crate::frame::Call;
use crate::generated::block::{
    KCOMP_BLOCK_CAPACITY_LEN, KCOMP_BLOCK_DEVICE_SECTOR, KCOMP_BLOCK_LBA_LEN,
    KCOMP_BLOCK_METHOD_CAPACITY, KCOMP_BLOCK_METHOD_READ, KCOMP_BLOCK_METHOD_WRITE,
};

/// 分派一次 `block.device` 调用：`port` 已由 image 级 switch（[`crate::kcomp_services!`]）
/// 选中本契约，`method` 在本函数里落到具体后端方法。
///
/// 返回 `0 / -errno`（provider 语义；Core 的传输状态在调用方单独编码）。
pub fn dispatch<P: BlockDeviceProvider>(p: &P, method: u32, call: Call<'_>) -> i32 {
    match method {
        KCOMP_BLOCK_METHOD_CAPACITY => capacity(p, call),
        KCOMP_BLOCK_METHOD_READ => read(p, call),
        KCOMP_BLOCK_METHOD_WRITE => write(p, call),
        // 能力缺失（不是畸形帧）：与 Core 对"没有 dispatcher"的档位一致。
        _ => Errno::ENOSYS.code(),
    }
}

fn capacity<P: BlockDeviceProvider>(p: &P, call: Call<'_>) -> i32 {
    if !call.args.is_empty()
        || !call.input.is_empty()
        || call.output.len() != KCOMP_BLOCK_CAPACITY_LEN
    {
        return Errno::EINVAL.code();
    }
    call.output
        .copy_from_slice(&p.capacity_sectors().to_le_bytes());
    0
}

fn read<P: BlockDeviceProvider>(p: &P, call: Call<'_>) -> i32 {
    let Some(lba) = decode_lba(call.args) else {
        return Errno::EINVAL.code();
    };
    if !call.input.is_empty() || !is_transfer_len(call.output.len()) {
        return Errno::EINVAL.code();
    }
    match p.read(lba, call.output) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

fn write<P: BlockDeviceProvider>(p: &P, call: Call<'_>) -> i32 {
    let Some(lba) = decode_lba(call.args) else {
        return Errno::EINVAL.code();
    };
    if !call.output.is_empty() || !is_transfer_len(call.input.len()) {
        return Errno::EINVAL.code();
    }
    match p.write(lba, call.input) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

/// `lba` 的 `args` 编码：一个 LE `u64`（与 C 包装逐字节一致，见 `kcomp_block.h`）。
pub(super) fn encode_lba(lba: u64) -> [u8; KCOMP_BLOCK_LBA_LEN] {
    lba.to_le_bytes()
}

/// `args` 解码：长度必须恰为 [`KCOMP_BLOCK_LBA_LEN`]。
pub(super) fn decode_lba(args: &[u8]) -> Option<u64> {
    if args.len() != KCOMP_BLOCK_LBA_LEN {
        return None;
    }
    let mut bytes = [0u8; KCOMP_BLOCK_LBA_LEN];
    bytes.copy_from_slice(args);
    Some(u64::from_le_bytes(bytes))
}

/// 传输长度合法性：非零且是 sector 的整数倍。
///
/// typed 前端（[`crate::block::client`]）与 provider 侧 dispatch 共用同一条规则
/// ——消费侧早拒与部署无关（Direct / Gate 行为一致）。
pub(super) fn is_transfer_len(len: usize) -> bool {
    len > 0 && len.is_multiple_of(KCOMP_BLOCK_DEVICE_SECTOR)
}

#[cfg(test)]
mod tests;
