//! `Endpoint<BlockDevice>` 的 typed 前端（consumer 侧）。
//!
//! 业务代码只见 [`Endpoint::read`] / [`Endpoint::write`] /
//! [`Endpoint::capacity_sectors`]——method 号、frame、传输机制全部隐藏。
//! 调用机制（Direct / Gate）由 Core 在 bind 时选定，本层只实现，不选择。
//!
//! # 错误分类（三类必须可区分）
//!
//! - [`InvokeError::Transport`]：Core 传输失败，provider **未被调用**；
//! - [`InvokeError::Method`]：provider 被调用并返回 `-errno`；
//! - [`InvokeError::InvalidReply`]：传输成功但回复不是 `0 / -errno`。

use crate::block::BlockDevice;
use crate::block::dispatch::{encode_lba, is_transfer_len};
use crate::call;
use crate::endpoint::{Endpoint, InvokeError};
use crate::errno::Errno;
use crate::generated::block::{
    KCOMP_BLOCK_CAPACITY_LEN, KCOMP_BLOCK_METHOD_CAPACITY, KCOMP_BLOCK_METHOD_READ,
    KCOMP_BLOCK_METHOD_WRITE,
};

impl Endpoint<BlockDevice> {
    /// 设备容量（单位：512 字节 sector）。
    pub fn capacity_sectors(&self) -> Result<u64, InvokeError> {
        let mut reply = [0u8; KCOMP_BLOCK_CAPACITY_LEN];
        invoke(self.id(), KCOMP_BLOCK_METHOD_CAPACITY, &[], &[], &mut reply)?;
        Ok(u64::from_le_bytes(reply))
    }

    /// 从 `lba` 读 `buf.len()` 字节到 `buf`（传输长度 = `buf.len()`）。
    pub fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), InvokeError> {
        if !is_transfer_len(buf.len()) {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        invoke(
            self.id(),
            KCOMP_BLOCK_METHOD_READ,
            &encode_lba(lba),
            &[],
            buf,
        )
    }

    /// 从 `buf` 写 `buf.len()` 字节到 `lba`（传输长度 = `buf.len()`）。
    pub fn write(&self, lba: u64, buf: &[u8]) -> Result<(), InvokeError> {
        if !is_transfer_len(buf.len()) {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        invoke(
            self.id(),
            KCOMP_BLOCK_METHOD_WRITE,
            &encode_lba(lba),
            buf,
            &mut [],
        )
    }
}

/// 传输状态 → [`InvokeError`] 的唯一翻译点（provider 回复绝不与传输失败混淆）。
fn invoke(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    fn endpoint() -> Endpoint<BlockDevice> {
        Endpoint::<BlockDevice>::from_id(7).expect("stub validate accepts id != 0 + block contract")
    }

    /// typed 前端把 `lba` 编成 `args`（LE）并路由到 read 方法——**与 C 包装
    /// `kcomp_block_read` 同字节**；output 长度就是传输长度。
    #[test]
    fn read_encodes_lba_and_passes_the_output_window() {
        let _guard = test_support::lock();
        test_support::reset_script();
        test_support::script_call(0, 0);

        let mut buf = [0u8; 512];
        endpoint().read(0x0102_0304_0506_0708, &mut buf).unwrap();

        let call = test_support::last_call().expect("stub recorded the call");
        assert_eq!(call.endpoint, 7);
        assert_eq!(call.method, KCOMP_BLOCK_METHOD_READ);
        assert_eq!(call.args, encode_lba(0x0102_0304_0506_0708));
        assert!(call.input.is_empty());
        assert_eq!(call.output_len, 512);
    }

    /// write：输入负载原样过线，output 必须为空（写没有输出区）。
    #[test]
    fn write_passes_the_input_window() {
        let _guard = test_support::lock();
        test_support::reset_script();
        test_support::script_call(0, 0);

        endpoint().write(9, &[0x5A; 512]).unwrap();
        let call = test_support::last_call().unwrap();
        assert_eq!(call.method, KCOMP_BLOCK_METHOD_WRITE);
        assert_eq!(call.args, encode_lba(9));
        assert_eq!(call.input, std::vec![0x5A; 512]);
        assert_eq!(call.output_len, 0);
    }

    /// capacity：8 字节 LE 回复解码成 `u64`。
    #[test]
    fn capacity_decodes_the_le_reply() {
        let _guard = test_support::lock();
        test_support::reset_script();
        test_support::script_call(0, 0);

        assert_eq!(
            endpoint().capacity_sectors().unwrap(),
            0x0102_0304_0506_0708
        );
        let call = test_support::last_call().unwrap();
        assert_eq!(call.method, KCOMP_BLOCK_METHOD_CAPACITY);
        assert!(call.args.is_empty());
        assert!(call.input.is_empty());
        assert_eq!(call.output_len, KCOMP_BLOCK_CAPACITY_LEN);
    }

    /// 传输失败 / 方法失败 / 无意义回复，三者必须可区分。
    #[test]
    fn transport_method_and_invalid_reply_are_distinguishable() {
        let _guard = test_support::lock();
        let mut buf = [0u8; 512];

        test_support::reset_script();
        test_support::script_call(-2, 0);
        assert_eq!(
            endpoint().read(0, &mut buf),
            Err(InvokeError::Transport(Errno::ENOENT))
        );

        test_support::reset_script();
        test_support::script_call(0, -5);
        assert_eq!(
            endpoint().read(0, &mut buf),
            Err(InvokeError::Method(Errno::EIO))
        );

        test_support::reset_script();
        test_support::script_call(0, 7);
        assert_eq!(endpoint().read(0, &mut buf), Err(InvokeError::InvalidReply));
    }

    /// 畸形长度在调用前就被 typed 前端挡下（不浪费一次传输）。
    #[test]
    fn malformed_transfer_length_is_rejected_without_a_call() {
        let _guard = test_support::lock();
        test_support::reset_script();
        test_support::script_call(0, 0);

        assert_eq!(
            endpoint().read(0, &mut []),
            Err(InvokeError::Method(Errno::EINVAL))
        );
        assert_eq!(
            endpoint().read(0, &mut [0u8; 513]),
            Err(InvokeError::Method(Errno::EINVAL))
        );
        assert_eq!(
            endpoint().write(0, &[]),
            Err(InvokeError::Method(Errno::EINVAL))
        );
        assert_eq!(
            endpoint().write(0, &[0u8; 513]),
            Err(InvokeError::Method(Errno::EINVAL))
        );
        assert!(
            test_support::last_call().is_none(),
            "畸形长度不得触发 kcore_endpoint_call"
        );
    }
}
