//! host 锚定测试：钉住 ABI 编码（`docs/driver-model.md` §6.2）。
//! Core 侧有对应测试 `component::export::tests::dma_direction_encoding_is_stable`。

#[test]
fn dma_direction_encoding_is_stable() {
    use crate::DmaDirection::{Bidirectional, FromDevice, ToDevice};
    assert_eq!(ToDevice.as_i32(), 0);
    assert_eq!(FromDevice.as_i32(), 1);
    assert_eq!(Bidirectional.as_i32(), 2);
}

/// InterfaceKind ABI 编码锚定（与 Core `export.rs::kind_from_u32` 一致）。
#[test]
fn interface_kind_encoding_is_stable() {
    use crate::binding::InterfaceKind::{Device, Policy, Service};
    assert_eq!(Device.as_u32(), 0);
    assert_eq!(Service.as_u32(), 1);
    assert_eq!(Policy.as_u32(), 2);
}

/// SchedulerPolicy ABI fingerprint 锚定：与 Core `sched::SCHEDULER_POLICY_ABI`
/// 必须是同一数值（A/B 双侧手工锚定）。
#[test]
fn scheduler_policy_abi_is_anchored() {
    assert_eq!(
        crate::binding::SCHEDULER_POLICY_ABI.raw(),
        0x5343_4845_4455_4C52
    );
}

/// driver.prober ABI fingerprint 锚定（ASCII "DRVPROBE"）：prober 与 driver
/// 由完全相同的契约编译——数值漂移会让 bind 直接拒绝，这里把它钉死。
#[test]
fn driver_prober_abi_is_anchored() {
    assert_eq!(
        crate::binding::DRIVER_PROBER_ABI.raw(),
        0x4452_5650_524F_4245
    );
}

/// 分配接口名字锚定：publish / bind 两侧必须逐字节一致。
#[test]
fn driver_prober_name_is_anchored() {
    assert_eq!(crate::binding::DRIVER_PROBER_NAME, b"driver.prober");
}

/// `report_attempt` outcome 编码锚定（0 = Match，1 = NoMatch）。
#[test]
fn assign_outcome_encoding_is_stable() {
    assert_eq!(crate::binding::ASSIGN_MATCH, 0);
    assert_eq!(crate::binding::ASSIGN_NO_MATCH, 1);
}

/// Trace ABI 布局锚定（编译期 `const _` 断言之外的 host 复核；与 Core
/// `trace::abi` 的布局测试同值，改了字段必须双侧同步）。
#[test]
fn trace_abi_layouts_are_anchored() {
    assert_eq!(core::mem::size_of::<crate::abi::TraceRecordAbi>(), 48);
    assert_eq!(core::mem::size_of::<crate::abi::TraceStatsAbi>(), 40);
    assert_eq!(core::mem::align_of::<crate::abi::TraceStatsAbi>(), 8);
}

/// `KcompCreateArgs` 布局锚定（host = 64-bit 指针 → 24 字节）：C 头文件
/// `struct KcompCreateArgs` 的 `_Static_assert` 与本测试必须同值；RV32 为 16。
#[test]
fn kcomp_create_args_layout_is_anchored() {
    use crate::abi::KcompCreateArgs;
    assert_eq!(core::mem::size_of::<KcompCreateArgs>(), 24);
    assert_eq!(core::mem::align_of::<KcompCreateArgs>(), 8);
    assert_eq!(core::mem::offset_of!(KcompCreateArgs, config), 8);
    assert_eq!(core::mem::offset_of!(KcompCreateArgs, config_len), 16);
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

/// `kcomp_abi` 指纹锚定（ASCII "KCOMPABI"）：Core 校验组件 ELF 里该符号的值，
/// 数值本身可当 8 字节大端 ASCII 读出来——两个断言同时钉死数值与 tag 拼写。
#[test]
fn kcomp_abi_fingerprint_is_anchored() {
    let abi = crate::abi::KCOMP_ABI;
    assert_eq!(abi, 0x4B43_4F4D_5041_4249);
    assert_eq!(&abi.to_be_bytes(), b"KCOMPABI");
}

/// `block.device` 名字 / kind 锚定：publish / bind 两侧必须逐字节一致，kind
/// 必须为 Device（Core 拒绝同名不同 kind）；改动必须是一次刻意的测试修改。
#[test]
fn block_device_name_and_kind_are_anchored() {
    use crate::binding::InterfaceKind::Device;
    use crate::binding::{BLOCK_DEVICE_NAME, BlockDevice};
    assert_eq!(BLOCK_DEVICE_NAME, b"block.device");
    assert_eq!(
        <BlockDevice as crate::binding::Service>::NAME,
        b"block.device"
    );
    assert_eq!(<BlockDevice as crate::binding::Service>::KIND, Device);
}

/// BlockDevice ABI fingerprint 锚定（ASCII "BLOCKDEV"）：数值本身可当 8 字节
/// 大端 ASCII 读出来——两个断言同时钉死数值与"它真的是那个 tag"。
#[test]
fn block_device_abi_is_anchored() {
    let abi = crate::binding::BLOCK_DEVICE_ABI.raw();
    assert_eq!(abi, 0x424C_4F43_4B44_4556);
    assert_eq!(&abi.to_be_bytes(), b"BLOCKDEV");
}

/// `BlockDeviceApi` 布局锚定：三个函数指针、无 padding（host = 64-bit 指针）。
/// exact ABI fingerprint 认的就是这份布局——字段增删必须同步改测试。
#[test]
fn block_device_api_layout_is_anchored() {
    assert_eq!(core::mem::size_of::<crate::binding::BlockDeviceApi>(), 24);
    assert_eq!(core::mem::align_of::<crate::binding::BlockDeviceApi>(), 8);
}

// ---------------------------------------------------------------------------
// block.device provider wrapper（`crate::block`）
// ---------------------------------------------------------------------------

use crate::block::{BlockDeviceProvider, BlockDeviceService};

/// wrapper 测试用 mock：`error == 0` → `Ok`（read 回填 0xA5），否则返回该 errno。
struct BlockMock {
    capacity: u64,
    error: i32,
}

impl BlockMock {
    const fn new(capacity: u64, error: i32) -> Self {
        Self { capacity, error }
    }
}

impl BlockDeviceProvider for BlockMock {
    fn capacity_sectors(&self) -> u64 {
        self.capacity
    }

    fn read(&self, _lba: u64, buf: &mut [u8]) -> Result<(), i32> {
        if self.error != 0 {
            return Err(self.error);
        }
        buf.fill(0xA5);
        Ok(())
    }

    fn write(&self, _lba: u64, _buf: &[u8]) -> Result<(), i32> {
        if self.error != 0 {
            return Err(self.error);
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

    fn read(&self, _lba: u64, _buf: &mut [u8]) -> Result<(), i32> {
        panic!("read must not be called for invalid args")
    }

    fn write(&self, _lba: u64, _buf: &[u8]) -> Result<(), i32> {
        panic!("write must not be called for invalid args")
    }
}

/// `const fn new` 直接做 `static` 初始化（本组测试的 static 都是编译期证据）；
/// 三个 table 指针非空且互不相同 = adapter 已按 `P` 单态化、没有静默指错。
#[test]
fn block_provider_table_is_complete_and_distinct() {
    static DEVICE: BlockDeviceService<BlockMock> = BlockDeviceService::new(BlockMock::new(8, 0));
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
    static OK: BlockDeviceService<BlockMock> = BlockDeviceService::new(BlockMock::new(2048, 0));
    static ERR: BlockDeviceService<BlockMock> = BlockDeviceService::new(BlockMock::new(2048, -5));

    let mut buf = [0u8; 512];
    let rc = unsafe { (OK.api().read)(OK.ctx(), 1, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(rc, 0);
    assert_eq!(buf, [0xA5; 512]);

    let mut buf = [0u8; 512];
    let rc = unsafe { (ERR.api().read)(ERR.ctx(), 1, buf.as_mut_ptr(), buf.len()) };
    assert_eq!(rc, -5);
}

/// write：`Ok` → `0`，`Err(e)` → `e`。
#[test]
fn block_provider_write_maps_ok_and_errno() {
    static OK: BlockDeviceService<BlockMock> = BlockDeviceService::new(BlockMock::new(2048, 0));
    static ERR: BlockDeviceService<BlockMock> = BlockDeviceService::new(BlockMock::new(2048, -5));

    let buf = [0x5Au8; 512];
    let ok = unsafe { (OK.api().write)(OK.ctx(), 1, buf.as_ptr(), buf.len()) };
    assert_eq!(ok, 0);
    let err = unsafe { (ERR.api().write)(ERR.ctx(), 1, buf.as_ptr(), buf.len()) };
    assert_eq!(err, -5);
}

/// 契约入参校验由 SDK 一次完成：null / `len == 0` / `len` 非 512 整数倍 →
/// `-EINVAL`（-22，与 Core `errno.rs` 一致），provider 完全不会被调用。
#[test]
fn block_provider_adapter_rejects_invalid_args_with_einval() {
    static NEVER: BlockDeviceService<NeverCalled> = BlockDeviceService::new(NeverCalled);
    let api = NEVER.api();
    let ctx = NEVER.ctx();
    let mut buf = [0u8; 512];

    assert_eq!(
        unsafe { (api.read)(ctx, 0, core::ptr::null_mut(), 512) },
        -22
    );
    assert_eq!(unsafe { (api.read)(ctx, 0, buf.as_mut_ptr(), 0) }, -22);
    assert_eq!(unsafe { (api.read)(ctx, 0, buf.as_mut_ptr(), 513) }, -22);
    assert_eq!(unsafe { (api.write)(ctx, 0, core::ptr::null(), 512) }, -22);
    assert_eq!(unsafe { (api.write)(ctx, 0, buf.as_ptr(), 0) }, -22);
    assert_eq!(unsafe { (api.write)(ctx, 0, buf.as_ptr(), 513) }, -22);
}
