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
//! prober 只交**分配数据**（`DeviceId`/`attempt`，不是 authority）；authority 仍由
//! 本驱动在**自己的 init 上下文**里向 Core claim，协议身份也由本驱动读设备头确认。
//! 若直接 `load virtio_blk` 而没有 prober，bind 失败并打印清晰错误（见 `kcomp_instance_create`）。
//! 没有支持的设备**不是失败**：init 返回 0、不 attach（干净的 no-device）。
//!
//! # 持久化：init 保留设备并 publish，exit 线性拆除
//!
//! fine match 通过后本驱动**不释放** claim：设备本体存进 `BLK`，init 末尾
//! `BLOCK_SERVICE.publish()`（staged：Core 在 `kcomp_instance_create` 返回 0 后提交），
//! 上层 Service 经 `block.device` bind 消费。停止（monitor `unload`）由
//! `kcomp_instance_destroy` 线性拆除：reset 设备 → drop 设备（释放队列 DMA）→ DMA 兜底 →
//! release MMIO（**最后**：Core 在还有 live DMA/IRQ 子 authority 时返回 `-EBUSY`）。
//!
//! # 状态与锁序（不变量）
//!
//! ```text
//! MMIO_HANDLE  AtomicUsize                    Core 的 MMIO handle（无锁单字）
//! BLK          Mutex<Option<VirtIOBlk<...>>>  设备本体；provider 唯一的锁
//! DMA_MAP      Mutex<[(u64, u64); SLOTS]>     paddr → DmaHandle
//! ```
//!
//! **锁序恒为 `BLK → DMA_MAP`，永不反向。** `VirtIOBlk::drop` 会经
//! `CoreHal::dma_dealloc` 去 `DMA_MAP.take`，所以 exit 里 drop 必须在 `BLK`
//! 锁内执行，顺序天然是 BLK → DMA_MAP。`MMIO_HANDLE` 刻意用原子而不是锁：
//! `CoreHal` 的 DMA 回调会在 `BLK` 锁内被调用（`dma_dealloc` 就是），Hal 侧
//! 不能再引入第二条锁 / 第二种锁序（`dma_alloc` 还要读它拿设备身份）。

#![no_std]

use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

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

/// MMIO handle（Core 的 opaque u64）。无锁单字：`CoreHal` 的 DMA 回调在 `BLK`
/// 锁内被调用，不能再取互斥锁；`AtomicU64` 在 riscv32imac 上不存在，所以用
/// `AtomicUsize`（rv64 = 8 字节；rv32 = 4 字节，只装得下低半 —— 本驱动单 claim、
/// slot=0 的场景够用）。
static MMIO_HANDLE: AtomicUsize = AtomicUsize::new(0);

/// paddr → DmaHandle 映射；`paddr == 0` = 空槽。
const SLOTS: usize = 16;
static DMA_MAP: Mutex<[(u64, u64); SLOTS]> = Mutex::new([(0, 0); SLOTS]);

/// 设备本体；`None` = 未 attach。provider 方法只取这一把锁。
///
/// `MmioTransport<'static>` 的论证见 `kcomp_instance_create` 里构造处的注释。
static BLK: Mutex<Option<VirtIOBlk<CoreHal, MmioTransport<'static>>>> = Mutex::new(None);

fn dma_insert(paddr: u64, handle: u64) {
    let mut slots = DMA_MAP.lock();
    for slot in slots.iter_mut() {
        if slot.0 == 0 {
            *slot = (paddr, handle);
            return;
        }
    }
    panic!("virtio_blk: DMA_MAP full");
}

fn dma_take(paddr: u64) -> Option<u64> {
    let mut slots = DMA_MAP.lock();
    for slot in slots.iter_mut() {
        if slot.0 == paddr {
            let handle = slot.1;
            *slot = (0, 0);
            return Some(handle);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// provider：零大小标记；状态全在 statics
// ---------------------------------------------------------------------------
//
// `BlockDeviceService<P>` 只是 provider 与生成的 `#[repr(C)]` table 的配对，
// **不是状态容器**：`P` 保持零大小，读写经 `BLK`。入参校验（null / len == 0 /
// 非 512 倍数 → `-EINVAL`）由 SDK adapter 统一完成，provider 不重复校验。

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
        let mut handle = 0u64;
        // MMIO_HANDLE 是无锁原子：Hal 回调可能在 BLK 锁内运行（见模块文档）。
        let rc = unsafe {
            abi::kcore_dma_alloc(
                MMIO_HANDLE.load(Ordering::SeqCst) as u64,
                size,
                dir_enc(direction).as_i32(),
                &mut handle,
            )
        };
        if rc != 0 {
            return (0, NonNull::dangling()); // virtio 约定：paddr == 0 = 分配失败
        }
        let (mut ptr, mut len, mut dev) = (0usize, 0usize, 0u64);
        let rc = unsafe { abi::kcore_dma_lease(handle, &mut ptr, &mut len, &mut dev) };
        if rc != 0 {
            // lease 失败：立刻归还刚拿到的 handle，不留悬空 authority。
            let _ = unsafe { abi::kcore_dma_release(handle) };
            return (0, NonNull::dangling());
        }

        // virtio 要求 DMA 清零；Core 的 dma_alloc 不清零。
        unsafe { core::ptr::write_bytes(ptr as *mut u8, 0, len) };
        dma_insert(dev, handle);
        (dev, NonNull::new(ptr as *mut u8).unwrap()) // v1 无 IOMMU：dev == pa == va
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        match dma_take(paddr) {
            Some(handle) => unsafe { abi::kcore_dma_release(handle) },
            None => panic!("virtio_blk: dma_dealloc: paddr not in DMA_MAP"),
        }
    }

    // MMIO 走 MmioTransport；本函数只有 PCI transport 才会调用。
    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        NonNull::new(paddr as *mut u8).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        buffer.as_ptr() as *mut u8 as usize as PhysAddr
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {}
}

// ---------------------------------------------------------------------------
// 入口 / 退出
// ---------------------------------------------------------------------------

kcomp_sdk::kcomp_instance_create!(|_args, _out_state| {
    // 设备选择由**协议无关**的 driver_prober 负责：它只做 compatible 级 coarse
    // candidate match，并把候选 DeviceId 放进 prober-owned cursor。本驱动 bind
    // `driver.prober`，逐台 claim + 做**自己的**协议级 fine match。
    //
    // 加载方式：先 `load scheduler_rr`（prober 的 dispatch 任务需要 SchedulerPolicy），
    // 再 `load driver_prober`（它在 Ready 后自动 load 本驱动）；直接 `load virtio_blk`
    // 而无 prober 时，下面给出明确错误并优雅失败。
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

        // 认领 prober 指出的**确切** DeviceId：authority 仍由本驱动向 Core claim，
        // prober 的分配接口不转移任何 authority。
        let mut mmio = 0u64;
        let rc = unsafe { abi::kcore_mmio_claim(device_id, &mut mmio) };
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

        // fine protocol match：claim 后读设备头确认协议身份（诊断 stale / 误选）。
        let mut virtio_device_id = 0u32;
        let rc = unsafe {
            abi::kcore_mmio_read_u32(mmio, VIRTIO_MMIO_DEVICE_ID_OFFSET, &mut virtio_device_id)
        };
        if rc != 0 || virtio_device_id != VIRTIO_ID_BLOCK {
            kcomp_sdk::klog!(
                "virtio_blk: device_id={} not a block device (id={}, rc={}); next",
                device_id,
                virtio_device_id,
                rc
            );
            let _ = unsafe { abi::kcore_mmio_release(mmio) };
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

        // MMIO_HANDLE 必须先于任何 dma_alloc 发布：CoreHal 用它给 DMA 认设备身份。
        MMIO_HANDLE.store(mmio as usize, Ordering::SeqCst);

        // lease MMIO：Core 校验过一次的 (ptr, len)（受信 KernelNative 直接访问）。
        let (mut base, mut region) = (0usize, 0usize);
        let rc = unsafe { abi::kcore_mmio_lease(mmio, &mut base, &mut region) };
        if rc != 0 {
            kcomp_sdk::klog!("virtio_blk: lease mmio failed (rc={})", rc);
            return rc;
        }

        let header = NonNull::new(base as *mut VirtIOHeader).unwrap();
        // SAFETY: `MmioTransport::new` 的 `'a` 是调用点自由选择的生命周期参数，
        // 这里显式选 `'static`：本组件实例在存活期间一直持有该 MMIO claim，
        // 且唯一持有 transport 的 `BLK` 在 exit 里先于 `kcore_mmio_release` 清空
        // （失败路径组件不再被调用，Core 兜底 revoke + quarantine），所以没有
        // 任何代码能在 lease 失效后碰到这块映射。
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

        // 保留设备：不 drop、不 release MMIO；状态进 static，随实例存活。
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
    let mmio = MMIO_HANDLE.load(Ordering::SeqCst) as u64;

    // 线性拆除（顺序即不变量，见模块文档）：
    //
    // 1) 复位设备：写 virtio-mmio DeviceStatus（0x070，4 字节）= 0。
    //    MMIO 此刻仍在 claim 内。
    if mmio != 0 {
        let _ = unsafe { abi::kcore_mmio_write_u32(mmio, VIRTIO_MMIO_STATUS_OFFSET, 0) };
    }

    // 2) 释放设备本体：`VirtIOBlk::drop` 的 dma_dealloc 在 BLK 锁内跑，
    //    锁序 BLK → DMA_MAP 保持不变（drop 是语句末尾才发生）。
    drop(BLK.lock().take());

    // 3) DMA 兜底：正常路径第 2 步已清空；这里扫掉异常路径残留的槽位。
    for slot in DMA_MAP.lock().iter_mut() {
        if slot.0 != 0 {
            let _ = unsafe { abi::kcore_dma_release(slot.1) };
            *slot = (0, 0);
        }
    }

    // 4) MMIO 最后释放：Core 在还有 live DMA/IRQ 子 authority 时拒绝（-EBUSY）。
    if mmio != 0 {
        let _ = unsafe { abi::kcore_mmio_release(mmio) };
    }

    // 5) 归零：之后再进入任何路径都不应看到这个 handle。
    MMIO_HANDLE.store(0, Ordering::SeqCst);

    // 6) 证据行（QEMU gate 靠它证明退出钩子真的执行了）。
    kcomp_sdk::klog!("[virtio_blk] exit");
    0
});
