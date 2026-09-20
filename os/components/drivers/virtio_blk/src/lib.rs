//! virtio_blk —— VirtIO-MMIO 块设备驱动组件（持久单设备，提供 `block.device`）。
//!
//! # 设备选择：协议无关 prober（coarse match）+ 驱动自己的 fine match
//!
//! 加载顺序：
//!
//! ```text
//! load scheduler_rr       # dispatch 任务需要绑定的 SchedulerPolicy
//! load driver_prober      # prober 枚举 compatible 候选 → 请求 Core 加载本驱动
//!   → (prober dispatch)   # prober Ready 后自动 load virtio_blk（一次）
//! load virtio_blk        # 本驱动 bind driver.prober → 逐台 claim + fine match
//! ```
//!
//! prober 只交**分配数据**（`DeviceId`/`attempt`）；本驱动在**自己的 init 上下文**
//! 里向 Core claim 确切的 `DeviceId`，协议身份也由本驱动读设备头确认。
//!
//! # 访问模型：claim 拿裸指针，steady state 不进 Core
//!
//! `kcore_device_claim` 返回本执行域下的 MMIO 指针（KernelNative = 寄存器基址）。
//! 之后 `MmioTransport` 直接 volatile 访问，Core 不参与每一次寄存器读写——不存在
//! `kcore_mmio_read_u32/write_u32` 这种 per-access 鉴权。
//!
//! # DMA：allocation 与 mapping 分离
//!
//! ```text
//! dma_alloc  = kcore_dma_alloc(内存) + kcore_dma_map(device_id, buffer)
//! share      = kcore_dma_map(device_id, buffer)      现有 buffer 映射给设备
//! unshare    = kcore_dma_unmap(mapping)
//! dma_dealloc= kcore_dma_unmap + kcore_dma_free
//! ```
//!
//! 组件内部用 `DMA_MAP: device_addr → mapping id` 做极薄 bookkeeping（`unshare`
//! 只收到 paddr）。No-IOMMU 下 device address 就是 buffer 地址。
//!
//! # 状态与锁序（不变量）
//!
//! ```text
//! DEVICE_ID    AtomicU32            Core 的设备身份（claim 锚点）
//! MMIO_BASE    AtomicUsize          claim 返回的寄存器基址（仅 destroy reset 用）
//! BLK          Mutex<Option<...>>   设备本体；provider 唯一的锁
//! DMA_MAP      Mutex<[(u64, u64)]>  device_addr → mapping id
//! ```
//!
//! **锁序恒为 `BLK → DMA_MAP`，永不反向。** `VirtIOBlk::drop` 会经 `CoreHal` 的
//! DMA 回调去 `DMA_MAP.take`，所以 exit 里 drop 必须在 `BLK` 锁内执行。
//! `DEVICE_ID` / `MMIO_BASE` 刻意用原子而不是锁：Hal 回调可能在 `BLK` 锁内运行。

#![no_std]

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use kcomp_sdk::binding::{ASSIGN_MATCH, ASSIGN_NO_MATCH, DriverProber, ServiceBinding};
use kcomp_sdk::block::{BlockDeviceProvider, BlockDeviceService};
use kcomp_sdk::{DmaDirection, abi};
use spin::Mutex;
use virtio_drivers::{
    BufferDirection, Hal, PAGE_SIZE, PhysAddr,
    device::blk::VirtIOBlk,
    transport::mmio::{MmioTransport, VirtIOHeader},
};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

// 组件不能依赖 os/core 的 errno 模块：本地按数值镜像（与 os/core/src/errno.rs 一致）。
const ENOENT: i32 = -2; // next_assignment 耗尽：没有更多分配
const EIO: i32 = -5; // virtio 传输失败
const ENODEV: i32 = -19; // 未 attach 就收到读写

// virtio-mmio 寄存器 offset（4 字节访问）。
const VIRTIO_MMIO_DEVICE_ID_OFFSET: u32 = 0x008;
const VIRTIO_MMIO_STATUS_OFFSET: u32 = 0x070;

const VIRTIO_ID_BLOCK: u32 = 2;

// ---------------------------------------------------------------------------
// 状态（全在 statics）
// ---------------------------------------------------------------------------

/// Core 的设备身份（claim 锚点）。DMA 映射与 IRQ 都用它。
static DEVICE_ID: AtomicU32 = AtomicU32::new(0);

/// claim 返回的寄存器基址；仅 destroy 时复位设备用（steady state 走 transport）。
static MMIO_BASE: AtomicUsize = AtomicUsize::new(0);

/// device_addr → mapping id；`device_addr == 0` = 空槽。
const SLOTS: usize = 16;
static DMA_MAP: Mutex<[(u64, u64); SLOTS]> = Mutex::new([(0, 0); SLOTS]);

/// 设备本体；`None` = 未 attach。provider 方法只取这一把锁。
static BLK: Mutex<Option<VirtIOBlk<CoreHal, MmioTransport<'static>>>> = Mutex::new(None);

fn dma_insert(device_addr: u64, mapping: u64) {
    let mut slots = DMA_MAP.lock();
    for slot in slots.iter_mut() {
        if slot.0 == 0 {
            *slot = (device_addr, mapping);
            return;
        }
    }
    panic!("virtio_blk: DMA_MAP full");
}

fn dma_take(device_addr: u64) -> Option<u64> {
    let mut slots = DMA_MAP.lock();
    for slot in slots.iter_mut() {
        if slot.0 == device_addr {
            let mapping = slot.1;
            *slot = (0, 0);
            return Some(mapping);
        }
    }
    None
}

fn device_id() -> u32 {
    DEVICE_ID.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// provider：零大小标记；状态全在 statics
// ---------------------------------------------------------------------------

struct VirtioBlkProvider;

impl BlockDeviceProvider for VirtioBlkProvider {
    fn capacity_sectors(&self) -> u64 {
        BLK.lock().as_ref().map_or(0, |blk| blk.capacity())
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), i32> {
        match BLK.lock().as_mut() {
            Some(blk) => blk.read_blocks(lba as usize, buf).map_err(|_| EIO),
            None => Err(ENODEV),
        }
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<(), i32> {
        match BLK.lock().as_mut() {
            Some(blk) => blk.write_blocks(lba as usize, buf).map_err(|_| EIO),
            None => Err(ENODEV),
        }
    }
}

/// publish 只在 init 末尾、且真的 attach 了设备时调用（staged：Core 在
/// `kcomp_instance_create` 返回 0 后提交）。
static BLOCK_SERVICE: BlockDeviceService<VirtioBlkProvider> =
    BlockDeviceService::new(VirtioBlkProvider);

// ---------------------------------------------------------------------------
// CoreHal：virtio-drivers 的 DMA/MMIO 回调 → `kcore_*` 白名单
// ---------------------------------------------------------------------------

struct CoreHal;

fn dir_enc(d: BufferDirection) -> DmaDirection {
    match d {
        BufferDirection::DriverToDevice => DmaDirection::ToDevice,
        BufferDirection::DeviceToDriver => DmaDirection::FromDevice,
        BufferDirection::Both => DmaDirection::Bidirectional,
    }
}

unsafe impl Hal for CoreHal {
    fn dma_alloc(pages: usize, direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let size = pages * PAGE_SIZE;
        let (mut ptr, mut len) = (core::ptr::null_mut(), 0usize);
        let rc = unsafe { abi::kcore_dma_alloc(size, &mut ptr, &mut len) };
        if rc != 0 {
            return (0, NonNull::dangling()); // virtio 约定：paddr == 0 = 分配失败
        }
        // virtio 要求 DMA 清零；Core 的 dma_alloc 不清零。
        unsafe { core::ptr::write_bytes(ptr, 0, len) };

        let (mut device_addr, mut mapping) = (0u64, 0u64);
        let rc = unsafe {
            abi::kcore_dma_map(
                device_id(),
                ptr,
                len,
                dir_enc(direction).as_i32(),
                &mut device_addr,
                &mut mapping,
            )
        };
        if rc != 0 {
            // 映射失败：立刻归还刚拿到的 allocation，不留悬空内存。
            let _ = unsafe { abi::kcore_dma_free(ptr) };
            return (0, NonNull::dangling());
        }
        dma_insert(device_addr, mapping);
        (device_addr, NonNull::new(ptr).unwrap()) // No-IOMMU：device_addr == ptr
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, vaddr: NonNull<u8>, _pages: usize) -> i32 {
        match dma_take(paddr) {
            Some(mapping) => {
                let _ = unsafe { abi::kcore_dma_unmap(mapping) };
                let _ = unsafe { abi::kcore_dma_free(vaddr.as_ptr()) };
                0
            }
            None => panic!("virtio_blk: dma_dealloc: paddr not in DMA_MAP"),
        }
    }

    // MMIO 走 MmioTransport；本函数只有 PCI transport 才会调用。
    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        NonNull::new(paddr as *mut u8).unwrap()
    }

    /// 把现有 buffer 映射给当前设备并返回设备可见地址。
    ///
    /// 这不是裸指针转换：经 Core 的 DMA mapping（No-IOMMU 下 device address 是
    /// identity，但 seam 已就位；未来 IOMMU/bounce buffer 在此变化）。
    unsafe fn share(buffer: NonNull<[u8]>, direction: BufferDirection) -> PhysAddr {
        let ptr = buffer.as_ptr() as *mut u8;
        let len = buffer.len();
        let (mut device_addr, mut mapping) = (0u64, 0u64);
        let rc = unsafe {
            abi::kcore_dma_map(
                device_id(),
                ptr,
                len,
                dir_enc(direction).as_i32(),
                &mut device_addr,
                &mut mapping,
            )
        };
        if rc != 0 {
            panic!("virtio_blk: share: kcore_dma_map failed rc={}", rc);
        }
        dma_insert(device_addr, mapping);
        device_addr
    }

    /// 撤销一次 `share` 建立的映射。
    unsafe fn unshare(paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {
        match dma_take(paddr) {
            Some(mapping) => {
                let _ = unsafe { abi::kcore_dma_unmap(mapping) };
            }
            None => panic!("virtio_blk: unshare: paddr not in DMA_MAP"),
        }
    }
}

// ---------------------------------------------------------------------------
// 入口 / 退出
// ---------------------------------------------------------------------------

kcomp_sdk::kcomp_instance_create!(|_args, _out_state| {
    // 设备选择由**协议无关**的 driver_prober 负责：它只做 compatible 级 coarse
    // candidate match，并把候选 DeviceId 放进 prober-owned cursor。本驱动 bind
    // `driver.prober`，逐台 claim + 做**自己的**协议级 fine match。
    let prober = match ServiceBinding::<DriverProber>::bind() {
        Ok(prober) => prober,
        Err(_) => {
            kcomp_sdk::klog!(
                "virtio_blk: no driver.prober provider; load scheduler_rr then driver_prober first"
            );
            return -1;
        }
    };

    const DRIVER_NAME: &[u8] = b"virtio_blk";

    // prober-owned cursor：逐台取候选设备；`-ENOENT` = 没有更多分配。
    loop {
        let mut attempt = 0u32;
        let mut device_id = 0u32;
        let rc = (prober.api().next_assignment)(
            prober.ctx(),
            DRIVER_NAME.as_ptr(),
            DRIVER_NAME.len(),
            &mut attempt,
            &mut device_id,
        );
        if rc != 0 {
            if rc != ENOENT {
                kcomp_sdk::klog!("virtio_blk: next_assignment failed (rc={})", rc);
            }
            break; // 分配耗尽（或错误）：没有更多设备。
        }

        // 认领 prober 指出的**确切** DeviceId：Core 记 owner 并返回可访问窗口。
        let (mut base, mut region) = (core::ptr::null_mut(), 0usize);
        let rc = unsafe { abi::kcore_device_claim(device_id, &mut base, &mut region) };
        if rc != 0 {
            kcomp_sdk::klog!(
                "virtio_blk: claim device_id={} failed (rc={}); next",
                device_id,
                rc
            );
            // outcome < 0 = 驱动无法完成检查（value = -Errno）；detail = 0。
            let _ = (prober.api().report_attempt)(prober.ctx(), attempt, rc, 0);
            continue;
        }

        // fine protocol match：claim 后直接读设备头确认协议身份（诊断 stale / 误选）。
        let virtio_device_id = unsafe {
            core::ptr::read_volatile(
                (base as usize + VIRTIO_MMIO_DEVICE_ID_OFFSET as usize) as *const u32,
            )
        };
        if virtio_device_id != VIRTIO_ID_BLOCK {
            kcomp_sdk::klog!(
                "virtio_blk: device_id={} not a block device (id={}); next",
                device_id,
                virtio_device_id
            );
            let _ = unsafe { abi::kcore_device_release(device_id) };
            let _ = (prober.api().report_attempt)(
                prober.ctx(),
                attempt,
                ASSIGN_NO_MATCH,
                virtio_device_id,
            );
            continue;
        }

        // 接受：保留 claim，报告 Match。
        let _ = (prober.api().report_attempt)(prober.ctx(), attempt, ASSIGN_MATCH, 0);

        // DEVICE_ID / MMIO_BASE 必须先于任何 dma_alloc 发布（CoreHal 用它们）。
        DEVICE_ID.store(device_id, Ordering::SeqCst);
        MMIO_BASE.store(base as usize, Ordering::SeqCst);

        let header = NonNull::new(base as *mut VirtIOHeader).unwrap();
        // SAFETY: `MmioTransport::new` 的 `'a` 是调用点自由选择的生命周期参数，
        // 这里显式选 `'static`：本组件实例在存活期间一直持有该 MMIO claim，
        // 且唯一持有 transport 的 `BLK` 在 exit 里先于 `kcore_device_release` 清空。
        let transport: MmioTransport<'static> = match unsafe { MmioTransport::new(header, region) }
        {
            Ok(transport) => transport,
            Err(error) => {
                kcomp_sdk::klog!("virtio_blk: MmioTransport::new failed: {:?}", error);
                return -1;
            }
        };

        let mut blk = match VirtIOBlk::<CoreHal, _>::new(transport) {
            Ok(blk) => blk,
            Err(error) => {
                kcomp_sdk::klog!("virtio_blk: VirtIOBlk::new failed: {:?}", error);
                return -1;
            }
        };
        kcomp_sdk::klog!("virtio_blk capacity: {} sectors", blk.capacity());

        // 健康检查：读 sector 0，确认设备真的能完成一次传输。
        let mut buf = [0u8; 512];
        if blk.read_blocks(0, &mut buf).is_err() {
            kcomp_sdk::klog!("read sector 0 failed");
            return -1;
        }
        let sig = u16::from_le_bytes([buf[510], buf[511]]);
        kcomp_sdk::klog!("mbr sig={:04x}", sig);
        if sig != 0xAA55 {
            kcomp_sdk::klog!("virtio_blk test failed");
            return -1;
        }

        // 保留设备：不 drop、不 release device；状态进 static，随实例存活。
        *BLK.lock() = Some(blk);

        // staged publish：Core 在 `kcomp_instance_create` 返回 0 后提交；失败 = init 失败。
        if let Err(rc) = BLOCK_SERVICE.publish() {
            kcomp_sdk::klog!("virtio_blk: publish block.device failed (rc={})", rc);
            return rc;
        }
        kcomp_sdk::klog!("virtio_blk test passed");
        break;
    }

    if BLK.lock().is_none() {
        // 没有支持的设备**不是失败**：组件仍进入 Ready（干净的 no-device）。
        kcomp_sdk::klog!("virtio_blk: no supported block device; init ok, no device attached");
    }
    0
});

kcomp_sdk::kcomp_instance_destroy!(|_state| {
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
    for slot in DMA_MAP.lock().iter_mut() {
        if slot.0 != 0 {
            let _ = unsafe { abi::kcore_dma_unmap(slot.1) };
            *slot = (0, 0);
        }
    }

    // 4) Device 最后释放：Core 在还有 live IRQ/DMA 时拒绝（-EBUSY）。
    if device_id != 0 {
        let _ = unsafe { abi::kcore_device_release(device_id) };
    }

    DEVICE_ID.store(0, Ordering::SeqCst);
    MMIO_BASE.store(0, Ordering::SeqCst);

    // 5) 证据行（QEMU gate 靠它证明退出钩子真的执行了）。
    kcomp_sdk::klog!("[virtio_blk] exit");
    0
});
