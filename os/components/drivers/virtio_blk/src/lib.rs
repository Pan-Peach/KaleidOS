#![no_std]

use core::cell::UnsafeCell;
use core::ptr::NonNull;

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
    // 设备发现 + 精确认领：纯枚举列出候选（不授权）→ 认领确切 DeviceId →
    // 读 VirtIO MMIO DeviceID（offset 0x008，2 = block）→ 不是块设备就释放、
    // 试下一个 ordinal。Core 不学 VirtIO 语义，读设备头只在驱动里。
    //
    // TODO(prober): 这个逐 ordinal 探测循环属于未来的组件级 prober（总线/协议
    // aware 的组件，不是 Core）。届时 prober 只把选中的 DeviceId 作为"选择数据"
    // 交给本驱动，本驱动仍在自己上下文里 claim 同一 DeviceId（见
    // docs/driver-model.md §9.1 / §12 Q1）。当前先内联最小实现以验证 ABI。
    const VIRTIO_MMIO_DEVICE_ID_OFFSET: u32 = 0x008;
    const VIRTIO_ID_BLOCK: u32 = 2;

    let mut ordinal = 0u32;
    let mut mmio = 0u64;
    loop {
        let mut device_id = 0u32;
        let rc = unsafe {
            abi::kcore_device_nth(b"virtio,mmio".as_ptr(), 11, ordinal, &mut device_id)
        };
        if rc != 0 {
            kcomp_sdk::klog!("no virtio block device found (device_nth: {})", rc);
            return rc;
        }

        let rc = unsafe { abi::kcore_mmio_claim(device_id, &mut mmio) };
        if rc != 0 {
            // 该 transport 已被别的 owner 认领：跳过，试下一台。
            kcomp_sdk::klog!("claim device_id={} failed: {}; trying next", device_id, rc);
            ordinal += 1;
            continue;
        }

        let mut virtio_device_id = 0u32;
        let rc = unsafe {
            abi::kcore_mmio_read_u32(mmio, VIRTIO_MMIO_DEVICE_ID_OFFSET, &mut virtio_device_id)
        };
        if rc != 0 {
            kcomp_sdk::klog!("read DeviceID failed: {}", rc);
            let _ = unsafe { abi::kcore_mmio_release(mmio) };
            return rc;
        }
        if virtio_device_id == VIRTIO_ID_BLOCK {
            break;
        }

        // 空 transport（DeviceID=0）或其他类型：释放这一台，继续探测。
        let _ = unsafe { abi::kcore_mmio_release(mmio) };
        ordinal += 1;
    }
    unsafe { *MMIO_HANDLE.0.get() = mmio; }

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

    // exit
    drop(blk);
    let _ = unsafe { abi::kcore_mmio_release(mmio) };
    if sig == 0xAA55 {
        kcomp_sdk::klog!("virtio_blk test passed");
        0
    } else {
        kcomp_sdk::klog!("virtio_blk test failed");
        -1
    }
});
