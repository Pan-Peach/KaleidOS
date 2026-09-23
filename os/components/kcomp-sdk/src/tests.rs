//! host 锚定测试：钉住 ABI 编码（`docs/architecture/driver-model.md` §6.2）。
//! Core 侧有对应测试 `component::export::tests::dma_direction_encoding_is_stable`。

mod macro_services;

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

/// SchedulerPolicy 身份锚定：`scheduler.policy` 契约的 name / ABI / contract /
/// method 数值漂移 = Core 选择直接拒绝（生成物是单一来源，这里钉死数值与拼写）。
/// 更完整的 wire 编解码锚定在 `crate::scheduler::tests`。
#[test]
fn scheduler_policy_identity_is_anchored() {
    use crate::scheduler::{
        SCHEDULER_METHOD_CHOOSE_NEXT, SCHEDULER_NONE, SCHEDULER_POLICY_ABI, SCHEDULER_POLICY_NAME,
        SCHEDULER_TASK_ID_LEN,
    };
    assert_eq!(SCHEDULER_POLICY_NAME, b"scheduler.policy");
    assert_eq!(SCHEDULER_POLICY_ABI.raw(), 0x5343_4845_4455_4C52);
    assert_eq!(SCHEDULER_METHOD_CHOOSE_NEXT, 0);
    assert_eq!(SCHEDULER_TASK_ID_LEN, 4);
    assert_eq!(SCHEDULER_NONE, u32::MAX);
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

#[test]
fn filesystem_abi_is_anchored() {
    assert_eq!(crate::binding::FILESYSTEM_ABI.raw(), 0x4649_4C45_5359_5354);
    assert_eq!(crate::binding::FILESYSTEM_NAME, b"filesystem");
    assert_eq!(crate::binding::FILESYSTEM_OPEN_READ, 1);
}

/// `report_attempt` outcome 编码锚定（0 = Match，1 = NoMatch）。
#[test]
fn assign_outcome_encoding_is_stable() {
    assert_eq!(crate::binding::ASSIGN_MATCH, 0);
    assert_eq!(crate::binding::ASSIGN_NO_MATCH, 1);
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

/// errno 数值的编译器级 pin。与 Core `kcomp_abi_drift.rs` 的抽查分工：Core 侧只有
/// `code()`，`from_code()` / `name()` 是 SDK 侧的解码路径，在这里独立钉死。
#[test]
fn errno_literals_are_pinned_to_stable_numbers() {
    assert_eq!(Errno::ENOENT as i32, 2);
    assert_eq!(Errno::EIO as i32, 5);
    assert_eq!(Errno::EBUSY as i32, 16);
    assert_eq!(Errno::ENODEV as i32, 19);
    assert_eq!(Errno::EINVAL as i32, 22);
    assert_eq!(Errno::EKEYREVOKED as i32, 128);
    // `from_code` 的输入是 ABI 返回形状（`0` / `-errno`）；正数不是合法输入，
    // 落回 EIO 兜底（与生成前行为逐位一致）。
    assert_eq!(Errno::from_code(-22), Errno::EINVAL);
    assert_eq!(Errno::from_code(22), Errno::EIO);
    assert_eq!(Errno::EINVAL.code(), -22);
    assert_eq!(Errno::EINVAL.name(), "EINVAL");
}
