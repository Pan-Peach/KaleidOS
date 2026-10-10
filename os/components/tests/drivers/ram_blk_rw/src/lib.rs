//! Writable per-instance RAM block provider with one owned IPC Server Task.
#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：提供裸机 #[panic_handler] 等。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

use kcomp_sdk::block::{BLOCK_DEVICE_NAME, BlockDevice, BlockDeviceProvider};
use kcomp_sdk::errno::{Errno, Result};
use kcomp_sdk::mem;
use kcomp_sdk::{kcomp_instance_create, kcomp_instance_destroy, klog};

/// 扇区大小（字节）——必须与 block 契约的 sector 一致。
const SECTOR: usize = 512;
/// 默认容量（扇区数）：256 × 512 B = 128 KiB。
const SECTORS: usize = 256;

/// State is borrowed only by this instance's serial Server Task.
#[repr(C)]
struct RamBlkRwState {
    sectors: u64,
    buf: *mut u8,
}

struct RamBlkRwProvider {
    state: *const RamBlkRwState,
}

impl RamBlkRwProvider {
    /// 把 `(lba, len)` 映射到本实例缓冲的字节区间；越界 / 溢出返回 `-EINVAL`。
    fn range(&self, lba: u64, len: usize) -> Result<(usize, usize)> {
        // SAFETY: state 指向本实例 create 时分配、存活期内地址稳定的 RamBlkRwState。
        let sectors = unsafe { (*self.state).sectors };
        let start = lba.checked_mul(SECTOR as u64).ok_or(Errno::EINVAL)?;
        let end = start.checked_add(len as u64).ok_or(Errno::EINVAL)?;
        if end > sectors * SECTOR as u64 {
            return Err(Errno::EINVAL);
        }
        Ok((start as usize, end as usize))
    }
}

impl BlockDeviceProvider for RamBlkRwProvider {
    fn capacity_sectors(&self) -> u64 {
        // SAFETY: state 在实例存活期内有效（见 range）。
        unsafe { (*self.state).sectors }
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        klog!("ram_blk_rw: read lba={} len={}", lba, buf.len());
        let (start, _) = self.range(lba, buf.len())?;
        // SAFETY: range() 保证 start..start+len 落在本实例缓冲内；调用方的 buf 与
        // 组件私有缓冲不重叠。
        unsafe {
            core::ptr::copy_nonoverlapping(
                (*self.state).buf.add(start),
                buf.as_mut_ptr(),
                buf.len(),
            );
        }
        Ok(())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<()> {
        klog!("ram_blk_rw: write lba={} len={}", lba, buf.len());
        let (start, _) = self.range(lba, buf.len())?;
        // SAFETY: 同 read；方向相反，仍不重叠。
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), (*self.state).buf.add(start), buf.len());
        }
        Ok(())
    }
}

extern "C" fn server(arg: *mut ()) {
    let owner = kcomp_sdk::management::current_component().unwrap();
    let endpoint =
        kcomp_sdk::endpoint::Endpoint::<BlockDevice>::lookup(owner, BLOCK_DEVICE_NAME).unwrap();
    let _ = kcomp_sdk::block::server::serve(&RamBlkRwProvider { state: arg.cast() }, endpoint.id());
    kcomp_sdk::management::exit_task();
}

kcomp_instance_create!(|_args, out_state| {
    // (1) 每实例缓冲：N 扇区。`mem_alloc` 首次交付零初始化（新实例 = 全新空白设备）。
    let buf_size = SECTOR * SECTORS;
    let buf_view = match mem::mem_alloc(buf_size as u64, 1) {
        Ok(view) => view,
        Err(_) => return Errno::ENOMEM.code(),
    };
    let buf = buf_view.base as *mut u8;

    // (2) 每实例 state：容量 + 缓冲指针。
    let state_size = core::mem::size_of::<RamBlkRwState>();
    let state_align = core::mem::align_of::<RamBlkRwState>();
    let state_view = match mem::mem_alloc(state_size as u64, state_align as u64) {
        Ok(view) => view,
        Err(_) => {
            let _ = mem::mem_release(buf_view);
            return Errno::ENOMEM.code();
        }
    };
    let state = state_view.base as *mut RamBlkRwState;
    // SAFETY: state 是 acquire 交付、对齐满足的 RamBlkRwState 存储；ptr::write 直接
    // 放置初始值（不读旧值）。
    unsafe {
        core::ptr::write(
            state,
            RamBlkRwState {
                sectors: SECTORS as u64,
                buf,
            },
        );
    }

    if let Err(error) = kcomp_sdk::endpoint::publish_ipc::<BlockDevice>(BLOCK_DEVICE_NAME) {
        let _ = mem::mem_release(buf_view);
        let _ = mem::mem_release(state_view);
        return error.code();
    }
    let mut task = 0;
    let rc = unsafe { kcomp_sdk::abi::kcore_task_create(server, state.cast(), &mut task) };
    if rc != 0 {
        let _ = mem::mem_release(buf_view);
        let _ = mem::mem_release(state_view);
        return rc;
    }
    let rc = unsafe { kcomp_sdk::abi::kcore_task_start(task) };
    // The Task record now retains arg. Failed Native backing stays resident.
    if rc != 0 {
        return rc;
    }

    // SAFETY: out_state 由 Core 保证可写（create 调用契约）。
    unsafe { *out_state = state.cast::<()>() };
    klog!("ram_blk_rw: endpoint published (sectors={})", SECTORS);
    0
});

kcomp_instance_destroy!(|_state| {
    // Task records and published Native backing remain resident.
    klog!("ram_blk_rw: destroy");
    0
});
