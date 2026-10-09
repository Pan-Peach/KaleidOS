//! virtio_blk —— VirtIO-MMIO 块设备驱动组件（`block.device` provider + `probe.result`）。
//!
//! # 设备选择：协议无关 prober（coarse match）+ 驱动自己的 fine match
//!
//! 加载顺序（**无环**；见 `docs/architecture/deployment.md` 与 `abi/probe.toml`）：
//!
//! ```text
//! load scheduler_rr       # dispatch 任务需要绑定的 SchedulerPolicy
//! load driver_prober      # prober 枚举 compatible 候选 → 逐台 create 本驱动
//!   → (prober dispatch)   # config = DriverCreateConfig{device_id, 结果端口名}
//! ```
//!
//! 本驱动**只从自己的 create config 读 assignment**（扁平字节：`device_id` +
//! `probe.result` 端口名），**绝不回调 prober**；`DeviceId` 只是选择数据，驱动在
//! **自己的 create 身份**下 `kcore_device_claim`，协议身份也由本驱动读设备头确认。
//! 结果（`Match` / `NoMatch`）写进**本实例** state，并 staged publish 到 config
//! 指定的 `probe.result` 端口；prober 在 create 返回 0 之后拉取。
//!
//! 每次 create 只处理**一台**设备：**Match** → claim + attach + `block.device`
//! endpoint + `probe.result` 同时发布，create 返回 0（`Match` 只在 attach 与两个
//! publication 都成功之后才写进结果）；**NoMatch** → release claim、只发布
//! `probe.result`（outcome = NoMatch）的 **report-only 实例**，create 仍返回 0；
//! **创建失败**（config 非法 / claim 失败 / attach 失败 / publish 失败 / panic）→
//! 走 create 边界回传错误，由 prober 单独记录为 creation failure。
//!
//! 访问与 DMA 走 Core 的既有机制（`kcore_device_claim` 返回本执行域 MMIO 指针，
//! steady state 不进 Core，无 per-access 鉴权；`kcore_dma_alloc` / `kcore_dma_map`
//! 分离，见 `docs/architecture/driver-model.md`），本驱动只做薄 bookkeeping。
//! No-IOMMU 下 device address 就是 buffer 地址。
//!
//! # 状态与锁序（不变量）
//!
//! ```text
//! DEVICE_ID    AtomicU32            Core 的设备身份（claim 锚点）
//! MMIO_BASE    AtomicUsize          claim 返回的寄存器基址（仅 destroy reset 用）
//! BLK          Mutex<Option<...>>   设备本体；provider 唯一的锁
//! DMA_MAP      Mutex<[(u64, u64)]>  device_addr → mapping id
//! instance     每实例 state          attached 标记 + probe.result 的 8 字节回复
//! ```
//!
//! **锁序恒为 `BLK → DMA_MAP`，永不反向。** `VirtIOBlk::drop` 会经 `CoreHal` 的
//! DMA 回调去 `DMA_MAP.take`，所以 exit 里 drop 必须在 `BLK` 锁内执行。
//! `DEVICE_ID` / `MMIO_BASE` 刻意用原子而不是锁：Hal 回调可能在 `BLK` 锁内运行。
//!
//! # 多设备模型（新：一个组件 = 一份独立 writable image）
//!
//! `DEVICE_ID` / `MMIO_BASE` / `DMA_MAP` / `BLK` 是**组件私有**的 statics：每次
//! instantiate 都重新放段 / 重定位，因此两个 `virtio_blk` 组件各自拥有独立的一套
//! static（独立设备身份 / MMIO 窗口 / DMA map / transport）。**一个设备 = 一个
//! 驱动组件 = 一个 `block.device` endpoint**；多设备 = 多次 instantiate。
//!
//! - `BLK.lock().is_some()` 现在只是**组件内**的二次 attach 守卫（一个组件不该
//!   attach 两台设备），不再阻止另一个组件 attach 另一台设备。
//! - `CoreHal` 的回调仍是**无上下文的**（`Hal` 不接收 per-instance ctx）；因为
//!   每个组件只 attach 一台设备，全局 `DEVICE_ID` 在组件内是唯一的，DMA 归属正确。
//! - prober 完成全部候选，为每台匹配设备创建独立驱动组件。

#![no_std]

mod hal;
mod state;

use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use hal::{CoreHal, DEVICE_ID, MMIO_BASE, device_id, rollback_attachment, unmap_residual_mappings};
use kcomp_sdk::abi;
use kcomp_sdk::block::{BLOCK_DEVICE_NAME, BlockDevice, BlockDeviceProvider};
use kcomp_sdk::errno::{Errno, Result};
use kcomp_sdk::frame::Call;
use kcomp_sdk::probe::{
    self, DriverCreateConfig, KCOMP_PROBE_RESULT_METHOD_RESULT, KCOMP_PROBE_RESULT_OUTPUT_LEN,
    ProbeReply,
};
use kcomp_sdk::{kcomp_instance_create, kcomp_instance_destroy, kcomp_services, klog};
use spin::Mutex;
use state::{VirtioBlkState, alloc_state, free_state};
use virtio_drivers::{
    device::blk::VirtIOBlk,
    transport::mmio::{MmioTransport, VirtIOHeader},
};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

// 组件不能依赖 os/core 的 errno 模块：本地按数值镜像（与 os/core/src/errno.rs 一致）。

// virtio-mmio 寄存器 offset（4 字节访问）。
const VIRTIO_MMIO_DEVICE_ID_OFFSET: u32 = 0x008;
const VIRTIO_MMIO_STATUS_OFFSET: u32 = 0x070;

const VIRTIO_ID_BLOCK: u32 = 2;

/// Gate dispatch token：`block.device` 契约（provider 私有；组合策略不需要知道）。
/// Gate dispatch token：`probe.result` 契约。
const PROBE_RESULT_PORT: u32 = 2;

// ---------------------------------------------------------------------------
// 状态（设备本体在 static；DMA 记账 / 设备身份的 HAL 细节在 `hal.rs`；
// 报告数据在 per-instance state）
// ---------------------------------------------------------------------------

/// 设备本体；`None` = 未 attach。provider 方法只取这一把锁。
static BLK: Mutex<Option<VirtIOBlk<CoreHal, MmioTransport<'static>>>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// provider：零大小标记；设备状态在 statics，报告数据在 per-instance state
// ---------------------------------------------------------------------------

struct VirtioBlkProvider;

impl BlockDeviceProvider for VirtioBlkProvider {
    fn capacity_sectors(&self) -> u64 {
        BLK.lock().as_ref().map_or(0, |blk| blk.capacity())
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let lba = usize::try_from(lba).map_err(|_| Errno::EOVERFLOW)?;
        match BLK.lock().as_mut() {
            Some(blk) => blk.read_blocks(lba, buf).map_err(|_| Errno::EIO),
            None => Err(Errno::ENODEV),
        }
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<()> {
        let lba = usize::try_from(lba).map_err(|_| Errno::EOVERFLOW)?;
        match BLK.lock().as_mut() {
            Some(blk) => blk.write_blocks(lba, buf).map_err(|_| Errno::EIO),
            None => Err(Errno::ENODEV),
        }
    }
}

/// The driver owns its Server Task and device; no Direct table is published.
extern "C" fn block_server(_arg: *mut ()) {
    let owner = kcomp_sdk::management::current_component().unwrap();
    let endpoint =
        kcomp_sdk::endpoint::Endpoint::<BlockDevice>::lookup(owner, BLOCK_DEVICE_NAME).unwrap();
    let _ = kcomp_sdk::block::server::serve(&VirtioBlkProvider, endpoint.id());
    kcomp_sdk::management::exit_task();
}

/// `probe.result` 的 `RESULT` 方法：把本实例的 8 字节回复写进 output。
///
/// 结果只属于本实例（report-only 实例也各有自己的 state），prober 在 create 返回
/// 0 之后拉取一次。
fn dispatch_probe_result(state: &VirtioBlkState, method: u32, call: Call<'_>) -> i32 {
    if method != KCOMP_PROBE_RESULT_METHOD_RESULT {
        return Errno::ENOSYS.code();
    }
    if !call.args.is_empty()
        || !call.input.is_empty()
        || call.output.len() != KCOMP_PROBE_RESULT_OUTPUT_LEN
    {
        return Errno::EINVAL.code();
    }
    call.output.copy_from_slice(&state.result);
    0
}

kcomp_services! {
    state: VirtioBlkState;
    PROBE_RESULT_PORT => dispatch_probe_result,
}

// ---------------------------------------------------------------------------
// 入口 / 退出
// ---------------------------------------------------------------------------

kcomp_instance_create!(|args, out_state| {
    // (1) assignment 只从 create config 来（扁平字节；**绝不回调 prober**）。
    // SAFETY: args 是 Core 提供的 create 参数指针（create ABI 契约：调用期间有效、
    // 可解引用）；config (ptr, len) 同样只在本次调用期间被借用。
    let assignment = match unsafe { DriverCreateConfig::from_create_args(&*args) } {
        Ok(assignment) => assignment,
        Err(error) => {
            klog!("virtio_blk: bad assignment create config: {:?}", error);
            return error.code();
        }
    };
    let device_id = assignment.device_id;
    klog!(
        "virtio_blk: assignment device_id={} endpoint={}",
        device_id,
        core::str::from_utf8(assignment.endpoint_name).unwrap_or("<name>")
    );

    // (2) 每实例 state：报告数据 + attached 标记（构造期清理由组件负责）。
    let state = alloc_state();
    if state.is_null() {
        klog!("virtio_blk: state allocation failed");
        return Errno::ENOMEM.code();
    }

    // (3) claim prober 指出的**确切** DeviceId：Core 记 owner 并返回访问窗口。
    //     principal = 本驱动（嵌套 create 的归属是驱动自己，不是 prober）。
    let (mut base, mut region) = (core::ptr::null_mut(), 0usize);
    let rc = unsafe { abi::kcore_device_claim(device_id, &mut base, &mut region) };
    if rc != 0 {
        klog!(
            "virtio_blk: claim device_id={} failed (rc={})",
            device_id,
            rc
        );
        unsafe { free_state(state) };
        return rc;
    }

    // (4) fine protocol match：claim 后直接读设备头确认协议身份（诊断 stale / 误选）。
    let virtio_device_id = unsafe {
        core::ptr::read_volatile(
            (base as usize + VIRTIO_MMIO_DEVICE_ID_OFFSET as usize) as *const u32,
        )
    };
    if virtio_device_id != VIRTIO_ID_BLOCK {
        klog!(
            "virtio_blk: device_id={} not a block device (id={}); reporting NoMatch",
            device_id,
            virtio_device_id
        );
        // report-only 实例：释放 claim、不发布 block endpoint；只发布结果端口。
        let _ = unsafe { abi::kcore_device_release(device_id) };
        // SAFETY: state 由本实例 create 分配、存活期地址稳定。
        unsafe { (*state).result = ProbeReply::no_match(virtio_device_id).encode() };
        if let Err(error) = unsafe {
            probe::publish_result_endpoint(
                assignment.endpoint_name,
                PROBE_RESULT_PORT,
                state.cast(),
            )
        } {
            klog!("virtio_blk: publish probe.result failed (rc={})", error);
            unsafe { free_state(state) };
            return error.code();
        }
        // SAFETY: out_state 由 Core 保证可写。
        unsafe { *out_state = state.cast::<()>() };
        klog!("virtio_blk: probe.result published (outcome=1)");
        return 0;
    }

    // (5) 组件内二次 attach 守卫：一个组件只 attach 一台设备。statics 是组件
    //     私有的（每次 instantiate 独立），因此这不阻止别的组件 attach 别的设备。
    if BLK.lock().is_some() {
        klog!(
            "virtio_blk: device already attached; rejecting second attachment (device_id={})",
            device_id
        );
        let _ = unsafe { abi::kcore_device_release(device_id) };
        unsafe { free_state(state) };
        return Errno::EBUSY.code();
    }

    // (6) attach：DEVICE_ID / MMIO_BASE 必须先于任何 dma_alloc 发布（CoreHal 用它们）。
    DEVICE_ID.store(device_id, Ordering::SeqCst);
    MMIO_BASE.store(base as usize, Ordering::SeqCst);

    let header = NonNull::new(base as *mut VirtIOHeader).unwrap();
    // SAFETY: `MmioTransport::new` 的 `'a` 是调用点自由选择的生命周期参数，
    // 这里显式选 `'static`：本组件实例在存活期间一直持有该 MMIO claim，
    // 且唯一持有 transport 的 `BLK` 在 exit 里先于 `kcore_device_release` 清空。
    let transport: MmioTransport<'static> = match unsafe { MmioTransport::new(header, region) } {
        Ok(transport) => transport,
        Err(error) => {
            klog!("virtio_blk: MmioTransport::new failed: {:?}", error);
            rollback_attachment(device_id);
            unsafe { free_state(state) };
            return Errno::EIO.code();
        }
    };

    let mut blk = match VirtIOBlk::<CoreHal, _>::new(transport) {
        Ok(blk) => blk,
        Err(error) => {
            klog!("virtio_blk: VirtIOBlk::new failed: {:?}", error);
            rollback_attachment(device_id);
            unsafe { free_state(state) };
            return Errno::EIO.code();
        }
    };
    klog!("virtio_blk capacity: {} sectors", blk.capacity());

    // 健康检查：读 sector 0，确认设备真的能完成一次传输。
    let mut buf = [0u8; 512];
    if blk.read_blocks(0, &mut buf).is_err() {
        klog!("virtio_blk: read sector 0 failed");
        drop(blk); // drop 需 DEVICE_ID 有效：必须在 rollback 之前。
        rollback_attachment(device_id);
        unsafe { free_state(state) };
        return Errno::EIO.code();
    }
    // 磁盘内容由上层 FS/分区组件解释；raw disk 也是合法块设备。

    // (7) Match **只在 attach + publication 都成功之后**成立：结果先写进本实例
    //     state，两个 endpoint 都 staged publish；任一失败都回滚（create 非 0 =
    //     Core 丢弃全部 pending，prober 记为 creation failure）。
    // SAFETY: state 由本实例 create 分配、存活期地址稳定。
    unsafe { (*state).result = ProbeReply::matched().encode() };
    if let Err(error) = kcomp_sdk::endpoint::publish_ipc::<BlockDevice>(BLOCK_DEVICE_NAME) {
        klog!("virtio_blk: publish block.device failed (rc={})", error);
        drop(blk);
        rollback_attachment(device_id);
        unsafe { free_state(state) };
        return error.code();
    }
    // SAFETY: state 由本实例 create 分配、存活期内地址稳定（Core 只存、不解引用；
    // Gate 分派把它原样交回 `dispatch_probe_result`）。
    if let Err(error) = unsafe {
        probe::publish_result_endpoint(assignment.endpoint_name, PROBE_RESULT_PORT, state.cast())
    } {
        klog!("virtio_blk: publish probe.result failed (rc={})", error);
        drop(blk);
        rollback_attachment(device_id);
        unsafe { free_state(state) };
        return error.code();
    }

    // 成功：保留设备（不 drop、不 release device；状态进 static，随实例存活）。
    *BLK.lock() = Some(blk);
    // SAFETY: state 由本实例 create 分配、存活期地址稳定。
    unsafe { (*state).attached = true };
    // SAFETY: out_state 由 Core 保证可写。
    unsafe { *out_state = state.cast::<()>() };
    if let Err(error) = kcomp_sdk::management::start_task(block_server) {
        // A Created Task may retain its image; failed Native backing stays resident.
        return error.code();
    }
    klog!("virtio_blk: probe.result published (outcome=0)");
    klog!("virtio_blk test passed");
    0
});

kcomp_instance_destroy!(|state| {
    // report-only 实例（NoMatch / 未 attach）：从未碰过全局设备状态，也**不得**复位
    // 已 attach 实例的设备——直接结束。state 存储按契约 §8 保留（不回收）。
    let attached = if state.is_null() {
        false
    } else {
        // SAFETY: state 是 create 写回的 opaque state（本组件类型），实例存活期内有效。
        unsafe { (*state.cast::<VirtioBlkState>()).attached }
    };
    if !attached {
        klog!("[virtio_blk] exit (report-only)");
        return 0;
    }

    let device_id = device_id();
    let base = MMIO_BASE.load(Ordering::SeqCst) as *mut u8;

    // 线性拆除（顺序即不变量）：
    //
    // 1) 复位设备：写 virtio-mmio DeviceStatus（0x070，4 字节）= 0。claim 仍有效。
    if !base.is_null() {
        unsafe {
            core::ptr::write_volatile(
                (base as usize + VIRTIO_MMIO_STATUS_OFFSET as usize) as *mut u32,
                0,
            );
        }
    }

    // 2) 释放设备本体：`VirtIOBlk::drop` 的 dma_dealloc（unmap + free）在 BLK 锁内跑，
    //    锁序 BLK → DMA_MAP 保持不变。
    drop(BLK.lock().take());

    // 3) DMA 兜底：正常路径第 2 步已清空；扫掉异常路径残留的映射（只 unmap；
    //    allocation 的回收由 Core 在组件失败/停止时兜底 quarantine）。
    unmap_residual_mappings();

    // 4) Device 最后释放：Core 在还有 live IRQ/DMA 时拒绝（-EBUSY）。
    if device_id != 0 {
        let _ = unsafe { abi::kcore_device_release(device_id) };
    }

    DEVICE_ID.store(0, Ordering::SeqCst);
    MMIO_BASE.store(0, Ordering::SeqCst);

    // 5) 证据行（QEMU gate 靠它证明退出钩子真的执行了）。
    klog!("[virtio_blk] exit");
    0
});
