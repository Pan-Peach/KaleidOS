//! virtio-drivers `Hal` adapter + DMA 记账（virtio_blk 私有）。
//!
//! 这是 virtio-drivers 与 `kcore_dma_*` 白名单之间的唯一胶水：`CoreHal` 把
//! `Hal::dma_alloc/share/unshare/dma_dealloc` 翻译成 Core 的 DMA allocation /
//! mapping 调用，并用 `DMA_MAP: device_addr → mapping id` 做极薄的反查记账。
//!
//! # 已知限制（本步不解决，如实登记）
//!
//! `Hal` 的语义约束（virtio-drivers 的 trait 形状）是**无状态的**：回调不接收
//! per-instance ctx。因此设备身份只能落在 image-global 的 [`DEVICE_ID`] 上——
//! DMA 归属、destroy 复位都锚在它。真正的 per-instance HAL（把 ctx 带进回调）是
//! 不声称多设备 / 多实例隔离。

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use kcomp_sdk::DmaDirection;
use kcomp_sdk::abi;
use spin::Mutex;
use virtio_drivers::{BufferDirection, Hal, PAGE_SIZE, PhysAddr};

/// Core 的设备身份（claim 锚点）。DMA 映射与 IRQ 都用它。
pub(crate) static DEVICE_ID: AtomicU32 = AtomicU32::new(0);

/// claim 返回的寄存器基址；仅 destroy 时复位设备用（steady state 走 transport）。
pub(crate) static MMIO_BASE: AtomicUsize = AtomicUsize::new(0);

/// device_addr → mapping id；`device_addr == 0` = 空槽。
const SLOTS: usize = 16;
static DMA_MAP: Mutex<[(u64, u64); SLOTS]> = Mutex::new([(0, 0); SLOTS]);

pub(crate) fn device_id() -> u32 {
    DEVICE_ID.load(Ordering::SeqCst)
}

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

/// destroy 兜底：正常路径 `VirtIOBlk::drop` 已清空映射；扫掉异常路径残留的映射
/// （只 unmap；allocation 的回收由 Core 在组件失败 / 停止时兜底 quarantine）。
pub(crate) fn unmap_residual_mappings() {
    for slot in DMA_MAP.lock().iter_mut() {
        if slot.0 != 0 {
            let _ = unsafe { abi::kcore_dma_unmap(slot.1) };
            *slot = (0, 0);
        }
    }
}

/// 构造期回滚：transport 初始化 / 自检 / publication 失败时，未进入 Ready 的实例
/// **不保留设备**——扫掉残留映射（只 unmap；allocation 由 Core 在失败时兜底
/// quarantine）、释放 claim、复位全局设备身份。
///
/// 调用时机：`blk`（若有）必须已 drop——`VirtIOBlk::drop` 的 `dma_dealloc` 需要
/// `DEVICE_ID` 仍然有效。
pub(crate) fn rollback_attachment(device_id: u32) {
    for slot in DMA_MAP.lock().iter_mut() {
        if slot.0 != 0 {
            let _ = unsafe { abi::kcore_dma_unmap(slot.1) };
            *slot = (0, 0);
        }
    }
    let _ = unsafe { abi::kcore_device_release(device_id) };
    DEVICE_ID.store(0, Ordering::SeqCst);
    MMIO_BASE.store(0, Ordering::SeqCst);
}

/// virtio-drivers 的 DMA / MMIO 回调 → `kcore_*` 白名单。
pub(crate) struct CoreHal;

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
    /// identity，但 seam 已就位；IOMMU/bounce buffer 在此变化）。
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
