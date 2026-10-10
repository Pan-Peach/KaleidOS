//! Read-only synthetic FAT12 block provider with one owned IPC Server Task.
#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：提供裸机 #[panic_handler] 等。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

use kcomp_sdk::block::{BLOCK_DEVICE_NAME, BlockDevice, BlockDeviceProvider};
use kcomp_sdk::errno::{Errno, Result};
use kcomp_sdk::{kcomp_instance_create, kcomp_instance_destroy, klog};

mod fat12;

/// 业务后端：内容 = 编译期生成的只读卷，因此 `&self` 方法无需锁。
struct RamBlkProvider;

impl BlockDeviceProvider for RamBlkProvider {
    fn capacity_sectors(&self) -> u64 {
        fat12::SECTORS as u64
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        klog!("ram_blk: read lba={} len={}", lba, buf.len());
        let start = usize::try_from(lba)
            .map_err(|_| Errno::EINVAL)?
            .checked_mul(fat12::SECTOR)
            .ok_or(Errno::EINVAL)?;
        let end = start.checked_add(buf.len()).ok_or(Errno::EINVAL)?;
        let bytes = fat12::IMAGE.get(start..end).ok_or(Errno::EINVAL)?;
        buf.copy_from_slice(bytes);
        Ok(())
    }

    fn write(&self, _lba: u64, _buf: &[u8]) -> Result<()> {
        // 只读卷：写请求按契约返回 -EROFS（FatFs 侧同样是 write-protected）。
        Err(Errno::EROFS)
    }
}

extern "C" fn server(_arg: *mut ()) {
    let owner = kcomp_sdk::management::current_component().unwrap();
    let endpoint =
        kcomp_sdk::endpoint::Endpoint::<BlockDevice>::lookup(owner, BLOCK_DEVICE_NAME).unwrap();
    let _ = kcomp_sdk::block::server::serve(&RamBlkProvider, endpoint.id());
    kcomp_sdk::management::exit_task();
}

kcomp_instance_create!(|_args, out_state| {
    if let Err(error) = kcomp_sdk::endpoint::publish_ipc::<BlockDevice>(BLOCK_DEVICE_NAME) {
        return error.code();
    }
    // This provider has no runtime state: the volume is immutable image data.
    unsafe { *out_state = core::ptr::null_mut() };
    if let Err(error) = kcomp_sdk::management::start_task(server) {
        return error.code();
    }
    klog!("ram_blk: endpoint published (sectors={})", fat12::SECTORS);
    0
});

kcomp_instance_destroy!(|_state| {
    klog!("ram_blk: destroy");
    0
});
