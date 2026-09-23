//! `Endpoint<BlockDevice>` 的 typed 前端（consumer 侧）+ Core 选定的调用绑定。
//!
//! 业务代码只见 [`BlockBinding::read`] / [`BlockBinding::write`] /
//! [`BlockBinding::capacity_sectors`]——method 号、frame、**调用机制**全部隐藏。
//! 调用机制（Direct / Gate）由 Core 在 [`Endpoint::bind`] 时选定一次，本层只实现，
//! 不选择：`Direct` 绑定直接调 provider function table（无 Core 介入、无分配、无
//! 打包），`Gate` 绑定走 `kcore_endpoint_call`——**调用点完全相同**。
//!
//! # 错误分类（三类必须可区分）
//!
//! - [`InvokeError::Transport`]：Core 传输失败（Gate 绑定才有），provider **未被调用**；
//! - [`InvokeError::Method`]：provider 被调用并返回 `-errno`（或请求按契约无效，
//!   前端直接挡下）；
//! - [`InvokeError::InvalidReply`]：传输成功但 provider / Core 的回复不是契约形状。

use crate::block::BlockDevice;
use crate::block::backend::{self, Backend};
use crate::block::dispatch::is_transfer_len;
use crate::endpoint::{Endpoint, InvokeError};
use crate::errno::Errno;

/// `block.device` 的**调用绑定**（consumer 侧句柄）。
///
/// 内部持有 Core 在 bind 时选定的机制（Direct：provider function table + state；
/// Gate：opaque EndpointId）——机制是**私有**的：消费者拿不到裸 function table，
/// 也无法选择走哪条路。
pub struct BlockBinding {
    backend: Backend,
}

impl Endpoint<BlockDevice> {
    /// bind：调 `kcore_endpoint_bind`——Core 做 **exact contract + abi + 存活**校验，
    /// 并按 `(caller domain, provider domain)` **一次性选定机制**（运行期不再重决策）。
    pub fn bind(&self) -> Result<BlockBinding, InvokeError> {
        Ok(BlockBinding {
            backend: backend::bind(self.id())?,
        })
    }
}

impl BlockBinding {
    /// 设备容量（单位：512 字节 sector）。
    pub fn capacity_sectors(&self) -> Result<u64, InvokeError> {
        backend::capacity_sectors(&self.backend)
    }

    /// 从 `lba` 读 `buf.len()` 字节到 `buf`（传输长度 = `buf.len()`）。
    pub fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), InvokeError> {
        if !is_transfer_len(buf.len()) {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        backend::read(&self.backend, lba, buf)
    }

    /// 从 `buf` 写 `buf.len()` 字节到 `lba`（传输长度 = `buf.len()`）。
    pub fn write(&self, lba: u64, buf: &[u8]) -> Result<(), InvokeError> {
        if !is_transfer_len(buf.len()) {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        backend::write(&self.backend, lba, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi;
    use crate::generated::block::{BlockDeviceApi, KCOMP_BLOCK_METHOD_READ};
    use crate::test_support;
    use core::sync::atomic::{AtomicU32, Ordering};

    /// Direct 路径的调用计数（"真的走了 function table"的证据）。
    static DIRECT_CALLS: AtomicU32 = AtomicU32::new(0);

    /// Direct function table 的替身：capacity 返回固定值，read 填 `0xA5`，write 计数。
    unsafe extern "C" fn direct_capacity(_ctx: *mut ()) -> u64 {
        DIRECT_CALLS.fetch_add(1, Ordering::SeqCst);
        64
    }

    unsafe extern "C" fn direct_read(_ctx: *mut (), _lba: u64, buf: *mut u8, len: usize) -> i32 {
        DIRECT_CALLS.fetch_add(1, Ordering::SeqCst);
        if buf.is_null() || len == 0 {
            return Errno::EINVAL.code();
        }
        // SAFETY: 调用方（SDK typed 前端）保证 buf 在调用期间可写、len 有效。
        unsafe { core::ptr::write_bytes(buf, 0xA5, len) };
        0
    }

    unsafe extern "C" fn direct_write(
        _ctx: *mut (),
        _lba: u64,
        _buf: *const u8,
        len: usize,
    ) -> i32 {
        DIRECT_CALLS.fetch_add(1, Ordering::SeqCst);
        if len == 0 { Errno::EINVAL.code() } else { 0 }
    }

    static DIRECT_TABLE: BlockDeviceApi = BlockDeviceApi {
        capacity_sectors: direct_capacity,
        read: direct_read,
        write: direct_write,
    };

    fn endpoint() -> Endpoint<BlockDevice> {
        Endpoint::<BlockDevice>::from_id(7).expect("stub validate accepts id != 0 + block contract")
    }

    /// 建立 Direct 绑定（Core 回复 DIRECT + 替身 table）。
    fn direct_binding() -> BlockBinding {
        let mut state = 0u8;
        let ctx = &mut state as *mut u8 as *mut ();
        test_support::script_bind(
            abi::KCORE_ENDPOINT_MECHANISM_DIRECT,
            &DIRECT_TABLE as *const BlockDeviceApi as usize,
            ctx as usize,
        );
        endpoint().bind().expect("stub bind returns DIRECT")
    }

    /// 建立 Gate 绑定（Core 回复 GATE，不交付 api/ctx）。
    fn gate_binding() -> BlockBinding {
        test_support::script_bind(abi::KCORE_ENDPOINT_MECHANISM_GATE, 0, 0);
        endpoint().bind().expect("stub bind returns GATE")
    }

    /// Direct：`read` / `capacity` / `write` 直调 function table，**绝不**经过
    /// `kcore_endpoint_call`（稳态零 Core 介入、零打包）。
    #[test]
    fn direct_binding_calls_the_function_table_without_core_involvement() {
        let _guard = test_support::lock();
        test_support::reset_script();
        test_support::script_call(0, 0);

        let before = DIRECT_CALLS.load(Ordering::SeqCst);
        let binding = direct_binding();

        let mut buf = [0u8; 512];
        binding.read(3, &mut buf).unwrap();
        assert_eq!(buf, [0xA5; 512], "Direct 读经过 provider function table");
        assert_eq!(binding.capacity_sectors().unwrap(), 64);
        binding.write(4, &[0x5A; 512]).unwrap();
        assert_eq!(DIRECT_CALLS.load(Ordering::SeqCst), before + 3);

        assert!(
            test_support::last_call().is_none(),
            "Direct 绑定绝不调用 kcore_endpoint_call"
        );
    }

    /// Gate：`read` 经 `kcore_endpoint_call`，args / output 与 Direct 同一扁平编码。
    #[test]
    fn gate_binding_routes_through_kcore_endpoint_call() {
        let _guard = test_support::lock();
        test_support::reset_script();
        test_support::script_call(0, 0);

        let binding = gate_binding();
        let mut buf = [0u8; 512];
        binding.read(0x0102_0304_0506_0708, &mut buf).unwrap();

        let call = test_support::last_call().expect("stub recorded the gate call");
        assert_eq!(call.endpoint, 7);
        assert_eq!(call.method, KCOMP_BLOCK_METHOD_READ);
        assert_eq!(call.args, 0x0102_0304_0506_0708u64.to_le_bytes());
        assert!(call.input.is_empty());
        assert_eq!(call.output_len, 512);
    }

    /// Gate：capacity 的 8 字节 LE 回复解码成 `u64`。
    #[test]
    fn gate_capacity_decodes_the_le_reply() {
        let _guard = test_support::lock();
        test_support::reset_script();
        test_support::script_call(0, 0);

        // stub 的 Gate capacity 回复固定为 0x0102_0304_0506_0708（LE）。
        assert_eq!(
            gate_binding().capacity_sectors().unwrap(),
            0x0102_0304_0506_0708
        );
        let call = test_support::last_call().unwrap();
        assert_eq!(
            call.method,
            crate::generated::block::KCOMP_BLOCK_METHOD_CAPACITY
        );
        assert_eq!(
            call.output_len,
            crate::generated::block::KCOMP_BLOCK_CAPACITY_LEN
        );
    }

    /// `Endpoint::bind` 把 **exact contract + abi** 交给 Core（SDK 不自行比较、
    /// 不自行发现）：`kcore_endpoint_bind` 记录的参数必须与契约身份逐位一致。
    #[test]
    fn bind_passes_the_contract_identity_to_core() {
        let _guard = test_support::lock();
        test_support::reset_script();
        let _ = direct_binding();

        let bind = test_support::last_bind().expect("stub recorded the bind");
        assert_eq!(bind.endpoint, 7);
        assert_eq!(
            bind.contract,
            <BlockDevice as crate::endpoint::Contract>::ID
        );
        assert_eq!(bind.abi, <BlockDevice as crate::endpoint::Contract>::ABI);
    }

    /// bind 的回复必须自洽：Direct 却没带 function table、未知机制编码 →
    /// `InvalidReply`（**绝不**降级或猜测）。
    #[test]
    fn bind_rejects_null_api_and_unknown_mechanism() {
        let _guard = test_support::lock();

        test_support::reset_script();
        test_support::script_bind(abi::KCORE_ENDPOINT_MECHANISM_DIRECT, 0, 0);
        assert_eq!(endpoint().bind().err(), Some(InvokeError::InvalidReply));

        test_support::reset_script();
        test_support::script_bind(0xDEAD, 0, 0);
        assert_eq!(endpoint().bind().err(), Some(InvokeError::InvalidReply));
    }

    /// bind 的传输失败（endpoint 已死 / contract 不符等）→ `Transport`。
    #[test]
    fn bind_transport_failure_maps_to_transport() {
        let _guard = test_support::lock();

        test_support::reset_script();
        test_support::script_bind_error(Errno::ENOENT.code());
        assert_eq!(
            endpoint().bind().err(),
            Some(InvokeError::Transport(Errno::ENOENT))
        );
    }

    /// 传输失败 / 方法失败 / 无意义回复，三者必须可区分（Gate 绑定）。
    #[test]
    fn transport_method_and_invalid_reply_are_distinguishable() {
        let _guard = test_support::lock();
        let mut buf = [0u8; 512];

        test_support::reset_script();
        test_support::script_call(-2, 0);
        assert_eq!(
            gate_binding().read(0, &mut buf),
            Err(InvokeError::Transport(Errno::ENOENT))
        );

        test_support::reset_script();
        test_support::script_call(0, -5);
        assert_eq!(
            gate_binding().read(0, &mut buf),
            Err(InvokeError::Method(Errno::EIO))
        );

        test_support::reset_script();
        test_support::script_call(0, 7);
        assert_eq!(
            gate_binding().read(0, &mut buf),
            Err(InvokeError::InvalidReply)
        );
    }

    /// Direct：provider 返回 `-errno` → `Method`；返回正数 → `InvalidReply`。
    #[test]
    fn direct_error_mapping_keeps_method_and_invalid_reply_apart() {
        let _guard = test_support::lock();

        unsafe extern "C" fn eio_read(_ctx: *mut (), _lba: u64, _buf: *mut u8, _len: usize) -> i32 {
            Errno::EIO.code()
        }
        unsafe extern "C" fn bogus_read(
            _ctx: *mut (),
            _lba: u64,
            _buf: *mut u8,
            _len: usize,
        ) -> i32 {
            7
        }
        static EIO_TABLE: BlockDeviceApi = BlockDeviceApi {
            capacity_sectors: direct_capacity,
            read: eio_read,
            write: direct_write,
        };
        static BOGUS_TABLE: BlockDeviceApi = BlockDeviceApi {
            capacity_sectors: direct_capacity,
            read: bogus_read,
            write: direct_write,
        };

        let mut buf = [0u8; 512];
        test_support::reset_script();
        test_support::script_bind(
            abi::KCORE_ENDPOINT_MECHANISM_DIRECT,
            &EIO_TABLE as *const BlockDeviceApi as usize,
            0,
        );
        assert_eq!(
            endpoint().bind().unwrap().read(0, &mut buf),
            Err(InvokeError::Method(Errno::EIO))
        );

        test_support::reset_script();
        test_support::script_bind(
            abi::KCORE_ENDPOINT_MECHANISM_DIRECT,
            &BOGUS_TABLE as *const BlockDeviceApi as usize,
            0,
        );
        assert_eq!(
            endpoint().bind().unwrap().read(0, &mut buf),
            Err(InvokeError::InvalidReply)
        );
    }

    /// 畸形长度在调用前就被 typed 前端挡下（不浪费一次传输，两条机制一致）。
    #[test]
    fn malformed_transfer_length_is_rejected_without_a_call() {
        let _guard = test_support::lock();
        test_support::reset_script();
        test_support::script_call(0, 0);
        let binding = gate_binding();

        assert_eq!(
            binding.read(0, &mut []),
            Err(InvokeError::Method(Errno::EINVAL))
        );
        assert_eq!(
            binding.read(0, &mut [0u8; 513]),
            Err(InvokeError::Method(Errno::EINVAL))
        );
        assert_eq!(
            binding.write(0, &[]),
            Err(InvokeError::Method(Errno::EINVAL))
        );
        assert_eq!(
            binding.write(0, &[0u8; 513]),
            Err(InvokeError::Method(Errno::EINVAL))
        );
        assert!(
            test_support::last_call().is_none(),
            "畸形长度不得触发 kcore_endpoint_call"
        );
    }
}
