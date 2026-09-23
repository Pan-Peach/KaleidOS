//! ram_blk —— **合成 RAM 块设备**（`block.device` provider）：Direct 调用机制的
//! 第一条完整链的 provider 侧。
//!
//! # 它证明什么
//!
//! 业务后端是 [`BlockDeviceProvider`]（与 virtio_blk 同形）；provider 在 create
//! 期间发布 endpoint，**同时**交付两种 transport（`docs/architecture/deployment.md`
//! §2）：
//!
//! - **Direct**：`BlockDeviceService` 生成的 `#[repr(C)]` function table + opaque
//!   `ctx`（`publish_endpoint` 交付，Core 只存、bind 时原样交给同域调用方）；
//! - **Gate**：`port` token + image 级 [`kcomp_services!`] dispatcher——Core 的
//!   `kcore_endpoint_call` 经它分派。
//!
//! **机制由 Core 在 bind 时选定**；provider 两种都提供，不选择。
//!
//! 内容来自 [`fat12`]（编译期生成的只读 FAT12 卷）：FatFs（C consumer）挂载它并
//! 读 `HELLO.TXT`——Direct 链上的字节正确性由 QEMU runner 逐字节比对。
//!
//! # 为什么不是 virtio_blk
//!
//! 本步只证明机制：合成设备没有 MMIO / DMA / 全局 statics / prober 重入问题。
//! 真实驱动迁移（per-instance HAL context）是后续步骤。

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

mod fat12;

/// provider 定义的端口 token（**Gate** 路径经 `kcomp_service_dispatch` 用它选中
/// 本契约；Direct 路径不使用它）。provider 私有——组合策略不需要知道。
const BLOCK_PORT: u32 = 1;

/// 业务后端：内容 = 编译期生成的只读卷，因此 `&self` 方法无需锁。
struct RamBlkProvider;

impl BlockDeviceProvider for RamBlkProvider {
    fn capacity_sectors(&self) -> u64 {
        fat12::SECTORS as u64
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        // 业务后端日志：Direct 与 Gate **共用**这条实现，因此它是"到达真实业务
        // 后端"的证据；机制证据由 gate dispatch 日志对照（runner 断言：业务 read
        // 不伴随任何 gate dispatch）。
        klog!("ram_blk: read lba={} len={}", lba, buf.len());
        let start = (lba as usize).saturating_mul(fat12::SECTOR);
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

/// provider 的 function table（不可变契约，image-global；`ctx` 指向实例状态）。
static SERVICE: BlockDeviceService<RamBlkProvider> = BlockDeviceService::new(RamBlkProvider);

/// 实例状态（`kcomp_instance_create` 写回的 opaque state）。
///
/// Gate dispatcher 要求非空 state（`kcomp_services!` 对 NULL 返回 `-EINVAL`），
/// 且它是**每实例**的：Direct 的 `ctx` 与它相互独立（前者是 provider 字段地址）。
#[repr(C)]
struct RamBlkState {
    sectors: u64,
}

/// Gate 侧入口：image 级 port switch（[`kcomp_services!`]）选中本契约后，落到与
/// Direct 相同的业务后端（[`block_dispatch`] 是 SDK 的 method switch）。
fn dispatch_gate(_state: &RamBlkState, method: u32, call: Call<'_>) -> i32 {
    // Gate 路径的机制证据：这行**只应**出现在显式 `kcore_endpoint_call` 上
    // （runner 断言业务 read 不伴随它）。
    klog!("ram_blk: gate dispatch method={}", method);
    block_dispatch::<RamBlkProvider>(&RamBlkProvider, method, call)
}

kcomp_services! {
    state: RamBlkState;
    BLOCK_PORT => dispatch_gate,
}

kcomp_instance_create!(|_args, out_state| {
    let size = core::mem::size_of::<RamBlkState>();
    let align = core::mem::align_of::<RamBlkState>();
    // SAFETY: 纯分配调用，无所有权语义；成功 = 对齐的 size 字节，失败 = NULL。
    let state = unsafe { abi::kcore_heap_alloc(size, align) };
    if state.is_null() {
        return Errno::ENOMEM.code();
    }
    // SAFETY: state 是刚分配、对齐满足、尚未初始化的 RamBlkState 存储。
    unsafe {
        core::ptr::write(
            state.cast::<RamBlkState>(),
            RamBlkState {
                sectors: fat12::SECTORS as u64,
            },
        );
    }

    // 发布 endpoint（staged：Core 在 create 返回 0 后原子提交）：
    // port_name = 契约名（单例固定名，组合策略据此发现），port = 本 provider 的
    // Gate dispatch token；api/ctx = Direct 的 function table + state。
    if let Err(error) = SERVICE.publish_endpoint(BLOCK_DEVICE_NAME, BLOCK_PORT) {
        // 发布失败：pending 未提交，Core 不调用 destroy——构造期清理由组件负责。
        // SAFETY: state 来自本次 create 的 kcore_heap_alloc（size/align 相同）。
        unsafe { abi::kcore_heap_dealloc(state, size, align) };
        return error.code();
    }

    // SAFETY: out_state 由 Core 保证可写（create 调用契约）。
    unsafe { *out_state = state.cast::<()>() };
    klog!("ram_blk: endpoint published (sectors={})", fat12::SECTORS);
    0
});

kcomp_instance_destroy!(|_state| {
    // phase 1：state 可能仍被消费者引用（Direct binding 的 ctx / 缓存），
    // 只逻辑停止、不回收存储（docs/architecture/component-lifecycle.md §8）。
    klog!("ram_blk: destroy");
    0
});
