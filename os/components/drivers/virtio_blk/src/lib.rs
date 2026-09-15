//! virtio_blk —— VirtIO-MMIO 块设备驱动组件。
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
//! 若直接 `load virtio_blk` 而没有 prober，bind 失败并打印清晰错误（见 `kcomp_init`）。
//!
//! # 本次迁移对 virtio_blk 的改动（逐条，便于 review）
//!
//! 1. 绑定契约：旧 prober 选择接口 → `DriverProber`/`next_assignment`；
//! 2. 单台选择 → `next_assignment` 循环（`-ENOENT` = 耗尽），逐台 claim；
//! 3. 协议不匹配时 `release` + `report_attempt(NoMatch)` 后试下一台；
//! 4. 分配耗尽（无支持的设备）时 init **返回 0**（`Ready`，但不 attach 设备）；
//! 5. `ASSIGN_MATCH` / claim 失败 / 读失败均 `report_attempt`；claim 失败不中止；
//! 6. transport / capacity / sector-0 读取**路径不变**；末尾仍 `release` MMIO
//!    （沿用既有 smoke 行为：不在 init 里保留一个 persistent `Bound` 设备）。

#![no_std]

use core::cell::UnsafeCell;
use core::ptr::NonNull;

use kcomp_sdk::binding::{ASSIGN_MATCH, ASSIGN_NO_MATCH, DriverProber, ServiceBinding};
use kcomp_sdk::{DmaDirection, abi};
use virtio_drivers::{
    BufferDirection, Hal, PAGE_SIZE, PhysAddr,
    device::blk::VirtIOBlk,
    transport::mmio::{MmioTransport, VirtIOHeader},
};

// MMIO handle：驱动单任务运行（IRQ 走 polled），且 rv32 目标没有 64-bit
// 原子（AtomicU64 在 riscv32imac 不存在）——用 UnsafeCell 存 u64，无新依赖。
struct MmioHandle(UnsafeCell<u64>);
unsafe impl Sync for MmioHandle {}
static MMIO_HANDLE: MmioHandle = MmioHandle(UnsafeCell::new(0));

// paddr -> DmaHandle. 驱动单任务运行，IRQ走polled，无需锁
// 若将来有并发，再换成细粒度锁
const SLOTS: usize = 16;
struct DmaMap(UnsafeCell<[(u64, u64); SLOTS]>); // (paddr, handle)
unsafe impl Sync for DmaMap {}
static DMA_MAP: DmaMap = DmaMap(UnsafeCell::new([(0, 0); SLOTS]));

impl DmaMap {
    fn insert(&self, paddr: u64, handle: u64) {
        let slots = unsafe { &mut *self.0.get() };
        for slot in slots.iter_mut() {
            if slot.0 == 0 {
                *slot = (paddr, handle);
                return;
            }
        }
        panic!("DMA map full");
    }

    fn take(&self, paddr: u64) -> Option<u64> {
        let slots = unsafe { &mut *self.0.get() };
        for slot in slots.iter_mut() {
            if slot.0 == paddr {
                let handle = slot.1;
                *slot = (0, 0);
                return Some(handle);
            }
        }
        None
    }
}

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
        let rc = unsafe {
            abi::kcore_dma_alloc(
                *MMIO_HANDLE.0.get(),
                size,
                dir_enc(direction).as_i32(),
                &mut handle,
            )
        };
        if rc != 0 {
            return (0, NonNull::dangling()); // 失败返回 (0, dangling)
        }
        let (mut ptr, mut len, mut dev) = (0usize, 0usize, 0u64);
        let rc = unsafe { abi::kcore_dma_lease(handle, &mut ptr, &mut len, &mut dev) };
        if rc != 0 {
            return (0, NonNull::dangling());
        }

        // virtio 要求 DMA 清零；Core 的 dma_alloc 不清零。
        unsafe { core::ptr::write_bytes(ptr as *mut u8, 0, len) };
        DMA_MAP.insert(dev, handle);
        (dev, NonNull::new(ptr as *mut u8).unwrap()) // dev==pa==va 暂时没有 iommu
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        match DMA_MAP.take(paddr) {
            Some(handle) => unsafe { abi::kcore_dma_release(handle) },
            None => {
                panic!("dma_dealloc: paddr not found");
            }
        }
    }

    // MMIO走MmioTransport， 此函数只有 PCI transport才会调用，暂时不实现
    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        NonNull::new(paddr as *mut u8).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        buffer.as_ptr() as *mut u8 as usize as PhysAddr
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {}
}

kcomp_sdk::kcomp_init!({
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

    const ENOENT: i32 = -2;
    const VIRTIO_MMIO_DEVICE_ID_OFFSET: u32 = 0x008;
    const VIRTIO_ID_BLOCK: u32 = 2;
    const DRIVER_NAME: &[u8] = b"virtio_blk";
    let mut attached = false;

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

        // 接受：保留 claim，报告 Match，走既有 transport / capacity / sector-0 路径。
        let _ = (prober.api().report_attempt)(prober.ctx(), attempt, ASSIGN_MATCH, 0);
        unsafe {
            *MMIO_HANDLE.0.get() = mmio;
        }

        // lease MMIO
        let (mut base, mut region) = (0usize, 0usize);
        let rc = unsafe { abi::kcore_mmio_lease(mmio, &mut base, &mut region) };
        if rc != 0 {
            kcomp_sdk::klog!("lease mmio failed: {}", rc);
            return rc;
        }

        // transport
        let header = NonNull::new(base as *mut VirtIOHeader).unwrap();
        let transport = match unsafe { MmioTransport::new(header, region) } {
            Ok(t) => t,
            Err(e) => {
                kcomp_sdk::klog!("MmioTransport::new failed: {:?}", e);
                return -1;
            }
        };
        let mut blk = match VirtIOBlk::<CoreHal, _>::new(transport) {
            Ok(b) => b,
            Err(e) => {
                kcomp_sdk::klog!("VirtIOBlk::new failed: {:?}", e);
                return -1;
            }
        };
        kcomp_sdk::klog!("virtio_blk capacity: {} sectors", blk.capacity());

        // 读 sector 0
        let mut buf = [0u8; 512];
        if blk.read_blocks(0, &mut buf).is_err() {
            kcomp_sdk::klog!("read sector 0 failed");
            return -1;
        }
        let sig = u16::from_le_bytes([buf[510], buf[511]]);
        kcomp_sdk::klog!("mbr sig={:04x}", sig);

        // exit：沿用既有 smoke 行为，init 末尾释放 MMIO（不保留 persistent Bound 设备）。
        drop(blk);
        let _ = unsafe { abi::kcore_mmio_release(mmio) };
        if sig != 0xAA55 {
            kcomp_sdk::klog!("virtio_blk test failed");
            return -1;
        }
        kcomp_sdk::klog!("virtio_blk test passed");
        attached = true;
        break;
    }

    if !attached {
        // 没有支持的设备**不是失败**：组件仍进入 Ready（干净的 no-device）。
        kcomp_sdk::klog!("virtio_blk: no supported block device; init ok, no device attached");
    }
    0
});

// TODO(component-exit): 退出收尾（停 DMA / mask IRQ / 释放 authority）——Core 只解析、从不调用，当前显式 no-op。
kcomp_sdk::kcomp_exit!(0);
