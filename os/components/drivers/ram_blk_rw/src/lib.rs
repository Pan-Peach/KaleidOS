//! ram_blk_rw —— **可写 RAM 块设备**（`block.device` provider）：[`ram_blk`] 的
//! 可写对偶。
//!
//! # 为什么需要它
//!
//! `ram_blk` 的内容是编译期生成的只读 FAT12 卷，`write` 恒返回 `-EROFS`。littlefs
//! 需要 prog + erase，因此要一个**可写**且**每实例独立**的后端：每个实例在
//! `kcomp_instance_create` 里经 Core 共享堆分配自己的 N 扇区**零初始化**缓冲，
//! 两个实例的存储互不影响。
//!
//! # 机制（与 ram_blk 同形）
//!
//! provider 在 create 期间发布 endpoint，**同时**交付两种 transport
//! （`docs/architecture/deployment.md` §2）：
//!
//! - **Direct**：`BlockDeviceService` 生成的 `#[repr(C)]` function table + opaque
//!   `ctx`；`ctx` 指向**本实例的 provider**，provider 携带本实例 state 指针，
//!   因此 Direct 的 read/write 直接落在本实例缓冲上；
//! - **Gate**：`port` token + image 级 [`kcomp_services!`] dispatcher——Core 的
//!   `kcore_endpoint_call` 经它分派，handler 从 Core 交回的 `&RamBlkRwState`
//!   构造同一个 provider。
//!
//! **机制由 Core 在 bind 时选定**；provider 两种都提供，不选择。
//!
//! # 与 ram_blk 的唯一结构差异：per-instance service
//!
//! ram_blk 的 provider 是**无状态**的（内容在 `const` 里），因此一个
//! `static SERVICE` 就够。本组件的 provider 必须携带**每实例** state 指针，而 SDK 的
//! `publish_endpoint` 把 Direct 的 `ctx` 固定为 `&self.provider`——image-global 的
//! static provider 无法区分实例。因此这里为每个实例在堆上分配一个
//! `BlockDeviceService<RamBlkRwProvider>` 并泄漏（phase 1 本就不回收实例存储，
//! 见 `docs/architecture/component-lifecycle.md` §8），使 `ctx` 指向本实例。

#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：提供裸机 #[panic_handler] 等。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

use kcomp_sdk::abi;
use kcomp_sdk::block::dispatch::dispatch as block_dispatch;
use kcomp_sdk::block::{BLOCK_DEVICE_NAME, BlockDeviceProvider, BlockDeviceService};
use kcomp_sdk::errno::{Errno, Result};
use kcomp_sdk::frame::Call;
use kcomp_sdk::{kcomp_instance_create, kcomp_instance_destroy, kcomp_services, klog};

/// provider 定义的端口 token（**Gate** 路径经 `kcomp_service_dispatch` 用它选中
/// 本契约；Direct 路径不使用它）。provider 私有——组合策略不需要知道。
const BLOCK_PORT: u32 = 1;

/// 扇区大小（字节）——必须与 block 契约的 sector 一致。
const SECTOR: usize = 512;
/// 默认容量（扇区数）：256 × 512 B = 128 KiB。
const SECTORS: usize = 256;

/// 实例状态（`kcomp_instance_create` 写回的 opaque state）。
///
/// Gate dispatcher 要求非空 state（`kcomp_services!` 对 NULL 返回 `-EINVAL`），
/// 且它是**每实例**的：`sectors` 是本实例容量，`buf` 指向本实例的堆缓冲。
/// Direct 的 `ctx` 不直接指向它，而是指向携带它的 per-instance provider。
#[repr(C)]
struct RamBlkRwState {
    sectors: u64,
    buf: *mut u8,
}

/// 业务后端：持有**本实例** state 指针，因此 Direct（`ctx` = `&self`）与 Gate
/// （`dispatch_gate` 从 state 构造）都落在同一份每实例存储上。
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
        // 业务后端日志：Direct 与 Gate **共用**这条实现，因此它是"到达真实业务
        // 后端"的证据；机制证据由 gate dispatch 日志对照。
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
            core::ptr::copy_nonoverlapping(
                buf.as_ptr(),
                (*self.state).buf.add(start),
                buf.len(),
            );
        }
        Ok(())
    }
}

/// Gate 侧入口：image 级 port switch（[`kcomp_services!`]）选中本契约后，落到与
/// Direct 相同的业务后端（[`block_dispatch`] 是 SDK 的 method switch）。
fn dispatch_gate(state: &RamBlkRwState, method: u32, call: Call<'_>) -> i32 {
    // Gate 路径的机制证据：这行**只应**出现在显式 `kcore_endpoint_call` 上。
    klog!("ram_blk_rw: gate dispatch method={}", method);
    block_dispatch::<RamBlkRwProvider>(&RamBlkRwProvider { state }, method, call)
}

kcomp_services! {
    state: RamBlkRwState;
    BLOCK_PORT => dispatch_gate,
}

kcomp_instance_create!(|_args, out_state| {
    // (1) 每实例缓冲：N 扇区，零初始化。新实例 = 全新空白设备。
    let buf_size = SECTOR * SECTORS;
    // SAFETY: 纯分配调用，无所有权语义；成功 = 对齐的 size 字节，失败 = NULL。
    let buf = unsafe { abi::kcore_heap_alloc(buf_size, 1) };
    if buf.is_null() {
        return Errno::ENOMEM.code();
    }
    // SAFETY: buf 是刚分配、恰好 buf_size 字节、尚未初始化的存储。
    unsafe { core::ptr::write_bytes(buf, 0, buf_size) };

    // (2) 每实例 state：容量 + 缓冲指针。
    let state_size = core::mem::size_of::<RamBlkRwState>();
    let state_align = core::mem::align_of::<RamBlkRwState>();
    // SAFETY: 同 (1) 的分配。
    let state = unsafe { abi::kcore_heap_alloc(state_size, state_align) };
    if state.is_null() {
        // SAFETY: buf 来自本次 create 的 kcore_heap_alloc（size/align 相同）。
        unsafe { abi::kcore_heap_dealloc(buf, buf_size, 1) };
        return Errno::ENOMEM.code();
    }
    // SAFETY: state 是刚分配、对齐满足、尚未初始化的 RamBlkRwState 存储。
    unsafe {
        core::ptr::write(
            state.cast::<RamBlkRwState>(),
            RamBlkRwState {
                sectors: SECTORS as u64,
                buf,
            },
        );
    }

    // (3) 每实例 service：Direct 的 `ctx` = `&provider`，provider 携带本实例 state，
    //     所以 Direct 的 read/write 落在本实例缓冲上（见文件头 §结构差异）。
    let service_size = core::mem::size_of::<BlockDeviceService<RamBlkRwProvider>>();
    let service_align = core::mem::align_of::<BlockDeviceService<RamBlkRwProvider>>();
    // SAFETY: 同 (1) 的分配。
    let service_ptr = unsafe { abi::kcore_heap_alloc(service_size, service_align) };
    if service_ptr.is_null() {
        // SAFETY: buf / state 来自本次 create 的 kcore_heap_alloc（size/align 相同）。
        unsafe {
            abi::kcore_heap_dealloc(buf, buf_size, 1);
            abi::kcore_heap_dealloc(state, state_size, state_align);
        }
        return Errno::ENOMEM.code();
    }
    // SAFETY: service_ptr 是刚分配、对齐满足、尚未初始化的 BlockDeviceService 存储。
    unsafe {
        core::ptr::write(
            service_ptr.cast::<BlockDeviceService<RamBlkRwProvider>>(),
            BlockDeviceService::new(RamBlkRwProvider { state: state.cast() }),
        );
    }
    // SAFETY: service 永不回收（phase 1 保留实例存储），'static 因此成立；provider
    // 内的 state 指针在实例存活期内有效。
    let service: &'static BlockDeviceService<RamBlkRwProvider> =
        unsafe { &*service_ptr.cast() };

    // 发布 endpoint（staged：Core 在 create 返回 0 后原子提交）：
    // port_name = 契约名（单例固定名，组合策略据此发现），port = 本 provider 的
    // Gate dispatch token；api/ctx = Direct 的 function table + 本实例 provider。
    if let Err(error) = service.publish_endpoint(BLOCK_DEVICE_NAME, BLOCK_PORT) {
        // 发布失败：pending 未提交，Core 不调用 destroy——构造期清理由组件负责。
        // SAFETY: 三个指针都来自本次 create 的 kcore_heap_alloc（size/align 相同）。
        unsafe {
            abi::kcore_heap_dealloc(buf, buf_size, 1);
            abi::kcore_heap_dealloc(state, state_size, state_align);
            abi::kcore_heap_dealloc(service_ptr, service_size, service_align);
        }
        return error.code();
    }

    // SAFETY: out_state 由 Core 保证可写（create 调用契约）。
    unsafe { *out_state = state.cast::<()>() };
    klog!("ram_blk_rw: endpoint published (sectors={})", SECTORS);
    0
});

kcomp_instance_destroy!(|_state| {
    // phase 1：state / 缓冲可能仍被消费者引用（Direct binding 的 ctx / 缓存），
    // 只逻辑停止、不回收存储（docs/architecture/component-lifecycle.md §8）。
    klog!("ram_blk_rw: destroy");
    0
});
