//! host 锚定测试：钉住 ABI 编码（`docs/architecture/driver-model.md` §6.2）。
//! Core 侧有对应测试 `component::export::tests::dma_direction_encoding_is_stable`。

mod heap;
mod macro_services;
mod mem;

#[test]
fn dma_direction_encoding_is_stable() {
    use crate::DmaDirection::{Bidirectional, FromDevice, ToDevice};
    assert_eq!(ToDevice.as_i32(), 0);
    assert_eq!(FromDevice.as_i32(), 1);
    assert_eq!(Bidirectional.as_i32(), 2);
}

/// `probe.result` 契约身份 / ABI 指纹 / create config 布局锚定（ASCII tag 的
/// 大端读数）：schema 数值漂移 = 组合期 `lookup` / `validate` 直接拒绝，这里钉死。
#[test]
fn probe_result_identity_is_anchored() {
    use crate::endpoint::Contract;
    use crate::probe::{
        KCOMP_DRIVER_CREATE_CONFIG_ABI, KCOMP_PROBE_OUTCOME_MATCH, KCOMP_PROBE_OUTCOME_NO_MATCH,
        KCOMP_PROBE_RESULT_ABI, KCOMP_PROBE_RESULT_CONTRACT, KCOMP_PROBE_RESULT_METHOD_RESULT,
        KCOMP_PROBE_RESULT_NAME, KCOMP_PROBE_RESULT_OUTPUT_LEN, ProbeResult,
    };
    assert_eq!(KCOMP_PROBE_RESULT_NAME, b"probe.result");
    assert_eq!(KCOMP_PROBE_RESULT_ABI, 0x5052_4F42_5253_4C54);
    assert_eq!(KCOMP_PROBE_RESULT_CONTRACT, 0x5052_4243_4F4E_5452);
    assert_eq!(KCOMP_DRIVER_CREATE_CONFIG_ABI, 0x4452_5643_4F4E_4647);
    assert_eq!(KCOMP_PROBE_RESULT_METHOD_RESULT, 0);
    assert_eq!(KCOMP_PROBE_RESULT_OUTPUT_LEN, 8);
    assert_eq!(KCOMP_PROBE_OUTCOME_MATCH, 0);
    assert_eq!(KCOMP_PROBE_OUTCOME_NO_MATCH, 1);
    assert_eq!(ProbeResult::ID, KCOMP_PROBE_RESULT_CONTRACT);
    assert_eq!(ProbeResult::ABI, KCOMP_PROBE_RESULT_ABI);
}

/// 生命周期入口的 Rust 镜像：函数指针 = 指针宽（C 侧 `KcompTaskEntry` /
/// `kcomp_instance_create` 的 ABI 宽度由这里钉死）。
#[test]
fn lifecycle_entry_types_are_anchored() {
    use crate::abi::{KcompInstanceCreate, KcompInstanceDestroy, KcompTaskEntry};
    let ptr = core::mem::size_of::<usize>();
    assert_eq!(core::mem::size_of::<KcompTaskEntry>(), ptr);
    assert_eq!(core::mem::size_of::<KcompInstanceCreate>(), ptr);
    assert_eq!(core::mem::size_of::<KcompInstanceDestroy>(), ptr);
}

/// 当前 exact ABI 指纹；Core 校验组件 ELF 里的同名符号。
#[test]
fn kcomp_abi_fingerprint_is_anchored() {
    let abi = crate::abi::KCOMP_ABI;
    assert_eq!(abi, 0x71A9_CE34_8D62_F0B5);
}

// ---------------------------------------------------------------------------
// block.device provider wrapper（`crate::block`）
// ---------------------------------------------------------------------------

use crate::block::{BlockDeviceProvider, BlockDeviceService};
use crate::errno::{Errno, Result};

/// wrapper 测试用 mock：`None` → `Ok`（read 回填 0xA5），`Some(e)` → `Err(e)`。
struct BlockMock {
    capacity: u64,
    error: Option<Errno>,
}

impl BlockMock {
    const fn new(capacity: u64, error: Option<Errno>) -> Self {
        Self { capacity, error }
    }
}

impl BlockDeviceProvider for BlockMock {
    fn capacity_sectors(&self) -> u64 {
        self.capacity
    }

    fn read(&self, _lba: u64, buf: &mut [u8]) -> Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        buf.fill(0xA5);
        Ok(())
    }

    fn write(&self, _lba: u64, _buf: &[u8]) -> Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(())
    }
}

/// 一被调用就 panic：证明 adapter 在 provider 之前挡下非法入参。
struct NeverCalled;

impl BlockDeviceProvider for NeverCalled {
    fn capacity_sectors(&self) -> u64 {
        0
    }

    fn read(&self, _lba: u64, _buf: &mut [u8]) -> Result<()> {
        panic!("read must not be called for invalid args")
    }

    fn write(&self, _lba: u64, _buf: &[u8]) -> Result<()> {
        panic!("write must not be called for invalid args")
    }
}

/// `const fn new` 直接做 `static` 初始化（本组测试的 static 都是编译期证据）；
/// 三个 table 指针非空且互不相同 = adapter 已按 `P` 单态化、没有静默指错。
#[test]
fn block_provider_table_is_complete_and_distinct() {
    static DEVICE: BlockDeviceService<BlockMock> = BlockDeviceService::new(BlockMock::new(8, None));
    let api = DEVICE.api();
    // capacity adapter 原样透传 provider 的值。
    assert_eq!(unsafe { (api.capacity_sectors)(DEVICE.ctx()) }, 8);
    let capacity = api.capacity_sectors as usize;
    let read = api.read as usize;
    let write = api.write as usize;
    assert_ne!(capacity, 0);
    assert_ne!(read, 0);
    assert_ne!(write, 0);
    assert_ne!(capacity, read);
    assert_ne!(capacity, write);
    assert_ne!(read, write);
}

/// read：`Ok` → `0`（数据写到调用方 buffer），`Err(e)` → `e`（`-Errno` 原样透传）。
#[test]
fn block_provider_read_maps_ok_and_errno() {
    static OK: BlockDeviceService<BlockMock> = BlockDeviceService::new(BlockMock::new(2048, None));
    static ERR: BlockDeviceService<BlockMock> =
        BlockDeviceService::new(BlockMock::new(2048, Some(Errno::EIO)));

    let mut buf = [0u8; 512];
    let rc = unsafe { (OK.api().read)(OK.ctx(), 1, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(rc, 0);
    assert_eq!(buf, [0xA5; 512]);

    let mut buf = [0u8; 512];
    let rc = unsafe { (ERR.api().read)(ERR.ctx(), 1, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(rc, Errno::EIO.code());
}

/// write：`Ok` → `0`，`Err(e)` → `e`。
#[test]
fn block_provider_write_maps_ok_and_errno() {
    static OK: BlockDeviceService<BlockMock> = BlockDeviceService::new(BlockMock::new(2048, None));
    static ERR: BlockDeviceService<BlockMock> =
        BlockDeviceService::new(BlockMock::new(2048, Some(Errno::EIO)));

    let buf = [0x5Au8; 512];
    let ok = unsafe { (OK.api().write)(OK.ctx(), 1, buf.as_ptr(), buf.len()) };
    assert_eq!(ok, 0);
    let err = unsafe { (ERR.api().write)(ERR.ctx(), 1, buf.as_ptr(), buf.len()) };
    assert_eq!(err, Errno::EIO.code());
}

/// 契约入参校验由 SDK 一次完成：null / `len == 0` / `len` 非 512 整数倍 →
/// `-EINVAL`（`Errno::EINVAL.code()`，与 Core `errno.rs` 一致），provider 完全不会被调用。
#[test]
fn block_provider_adapter_rejects_invalid_args_with_einval() {
    static NEVER: BlockDeviceService<NeverCalled> = BlockDeviceService::new(NeverCalled);
    let api = NEVER.api();
    let ctx = NEVER.ctx();
    let mut buf = [0u8; 512];

    assert_eq!(
        unsafe { (api.read)(ctx, 0, core::ptr::null_mut(), 512) },
        Errno::EINVAL.code()
    );
    assert_eq!(
        unsafe { (api.read)(ctx, 0, buf.as_mut_ptr(), 0) },
        Errno::EINVAL.code()
    );
    assert_eq!(
        unsafe { (api.read)(ctx, 0, buf.as_mut_ptr(), 513) },
        Errno::EINVAL.code()
    );
    assert_eq!(
        unsafe { (api.write)(ctx, 0, core::ptr::null(), 512) },
        Errno::EINVAL.code()
    );
    assert_eq!(
        unsafe { (api.write)(ctx, 0, buf.as_ptr(), 0) },
        Errno::EINVAL.code()
    );
    assert_eq!(
        unsafe { (api.write)(ctx, 0, buf.as_ptr(), 513) },
        Errno::EINVAL.code()
    );
}

/// errno 的 **SDK 侧解码路径**（`from_code()` / `name()`）锚定：Core 的
/// `code()` 与数值本身由 `kcomp_abi_drift.rs` / `make abi-check` 守卫。
#[test]
fn errno_decoding_keeps_its_contract() {
    // `from_code` 的输入是 ABI 返回形状（`0` / `-errno`）；正数不是合法输入，
    // 落回 EIO 兜底（与生成前行为逐位一致）。
    assert_eq!(Errno::from_code(-22), Errno::EINVAL);
    assert_eq!(Errno::from_code(22), Errno::EIO);
    assert_eq!(Errno::EINVAL.code(), -22);
    assert_eq!(Errno::EINVAL.name(), "EINVAL");
}
