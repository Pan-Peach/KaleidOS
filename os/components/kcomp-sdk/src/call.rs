//! Endpoint call 的**原始** SDK 包装（call ABI 管道；typed `Endpoint<C>` 在迁移
//! 步骤，不在本层）。
//!
//! 这一层只做两件事：
//!
//! 1. [`endpoint_call`]：把 `kcore_endpoint_call` 的裸指针 ABI 收成
//!    `(&[u8], &[u8], &mut [u8]) -> Result<i32>`——`Ok` 侧是 **provider 自己的
//!    status**；Core 传输失败翻成 `Err(Errno)`。两者在 ABI 上分离
//!    （`abi/core.toml` 的 `kcore_endpoint_call`），因此 provider 返回的负 errno
//!    不会变成传输失败，Core 的失败也不会被当成 provider 的返回值。
//! 2. [`frame`]：安全构造 flat [`KcompCallFrame`]（三个借用切片 → 三个
//!    `(ptr, len)` 对）。
//!
//! **不做**方法编号分配 / 参数编解码 / typed 契约——那是迁移步骤里
//! `Endpoint<C>` 的职责；本层不发明协议。

use crate::abi::{self, KcompCallFrame};
use crate::errno::{Errno, Result};

/// 调用一个 endpoint：`args` / `input` 只读，`output` 可写。
///
/// 成功 = `Ok(provider_status)`——**provider 自己的** `i32` 返回（`0 / -errno`，
/// 语义由契约定义）；Core 传输失败 = `Err(Errno)`（此时 provider 未被调用）。
///
/// `endpoint` 是组合期发现（`kcore_endpoint_lookup`）得到的 opaque `EndpointId`。
pub fn endpoint_call(
    endpoint: u64,
    method: u32,
    args: &[u8],
    input: &[u8],
    output: &mut [u8],
) -> Result<i32> {
    let mut provider_status = 0i32;
    // SAFETY: 三个切片在本帧内有效，Core 只在调用期间借用它们的 (ptr, len)；
    // `&mut provider_status` 指向本帧的栈变量。Core 不解析 payload 字节。
    let status = unsafe {
        abi::kcore_endpoint_call(
            endpoint,
            method,
            args.as_ptr(),
            args.len(),
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
            &mut provider_status,
        )
    };
    if status == 0 {
        Ok(provider_status)
    } else {
        Err(Errno::from_code(status))
    }
}

/// 安全构造 flat call frame：三个借用切片 → 三个 `(ptr, len)` 对。
///
/// 返回的 frame **不携带生命周期**（ABI 类型不能带借用）：它只在 `args` /
/// `input` / `output` 的借用仍然有效期间可用——provider dispatcher 的调用窗口，
/// Core 保证只在该窗口内借用。空切片得到"长度 0 + 非空 dangling 指针"，与
/// "无负载"语义一致（Core 只拒绝"长度非零 + 空指针"）。
pub fn frame(args: &[u8], input: &[u8], output: &mut [u8]) -> KcompCallFrame {
    KcompCallFrame {
        args: args.as_ptr(),
        args_len: args.len(),
        input: input.as_ptr(),
        input_len: input.len(),
        output: output.as_mut_ptr(),
        output_len: output.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `frame` helper：切片 → (ptr, len) 逐字段对应；空切片是"长度 0 + 非空指针"。
    #[test]
    fn frame_helper_wires_slices_to_pointer_length_pairs() {
        let args = [1u8, 2, 3];
        let input = [4u8, 5];
        let mut output = [0u8; 4];

        let built = frame(&args, &input, &mut output);
        assert_eq!(built.args, args.as_ptr());
        assert_eq!(built.args_len, args.len());
        assert_eq!(built.input, input.as_ptr());
        assert_eq!(built.input_len, input.len());
        assert_eq!(built.output, output.as_mut_ptr());
        assert_eq!(built.output_len, output.len());

        // 空负载：长度 0、指针非空（Core 只拒绝"长度非零 + 空指针"）。
        let empty = frame(&[], &[], &mut []);
        assert_eq!(empty.args_len, 0);
        assert_eq!(empty.input_len, 0);
        assert_eq!(empty.output_len, 0);
        assert!(!empty.args.is_null());
        assert!(!empty.output.is_null());
    }
}
