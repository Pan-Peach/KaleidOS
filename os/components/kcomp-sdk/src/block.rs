//! `block.device` 契约（KIND = Device）+ provider 侧 ergonomic wrapper。
//!
//! 本模块是 block 契约的**语义**住处：契约类型（[`BlockDevice`]）与 provider 包装
//! （[`BlockDeviceProvider`] / [`BlockDeviceService`]）放在一起；`#[repr(C)]`
//! function table（[`BlockDeviceApi`]）与名字 / 指纹 / sector 常量由
//! `tools/kabi/kabi_gen.py` 从 `abi/block.toml` 生成（[`crate::generated::block`]），
//! 这里 re-export。
//!
//! # provider 不写 `unsafe extern "C"`
//!
//! function table 的布局、指针形态、`0/-errno` 编码都是 ABI 契约的一部分。让每个
//! 驱动手写这张表，等于把 ABI 不变量和**裸指针前置条件**复制到每个 provider，写错
//! 一处就是 UB。包装把这两件事收回 SDK：
//!
//! ```text
//! impl BlockDeviceProvider for MyDevice { ... }   // provider：纯 Rust，无 unsafe
//! static DEVICE: BlockDeviceService<MyDevice> =
//!     BlockDeviceService::new(MyDevice { ... });
//! DEVICE.publish_endpoint(b"block.device", PORT)?;  // endpoint 模型（Direct + Gate）
//! ```
//!
//! [`BlockDeviceService::new`] 用单态化 adapter 从 `P` 生成 table；adapter 统一执行
//! 契约的入参校验（null / 空 / 非 512 倍数 → `-EINVAL`）并把裸指针收窄成
//! `&[u8]` / `&mut [u8]`；[`BlockDeviceService::publish_endpoint`] 的安全性论证见
//! 其文档。consumer 侧见 [`client::BlockBinding`]（Core 在 bind 时选定机制）。

use crate::abi::InterfaceKind;
use crate::endpoint::Contract;
use crate::errno::{Errno, Result};

// Gate / Direct 两条部署路径共用同一份业务后端（`BlockDeviceProvider`）：
//   - Direct：本文件下方的 `BlockDeviceService`（`#[repr(C)]` function table）；
//   - Gate  ：`block::dispatch`（扁平 frame → 同一个 provider 方法）。
// 业务后端不感知部署（docs/architecture/deployment.md §4）。
//
// `backend` 是**私有的调用后端**（Core 在 bind 时选定机制）；`client` 是 consumer
// 侧的 typed 前端（`BlockBinding`）。provider 发布 endpoint 用
// [`BlockDeviceService::publish_endpoint`]。
mod backend;
pub mod client;
pub mod dispatch;

#[cfg(test)]
pub(crate) mod tests_support;

// -----------------------------------------------------------------------
// 契约：block.device —— 驱动提供的 Device Interface（provider: virtio_blk）
// -----------------------------------------------------------------------
//
// docs/architecture/driver-model.md §9.1 ⑤：驱动 claim 完 MmioHandle / IrqHandle / DmaHandle
// 后向 Component Endpoint Registry 发布本接口，供上层 Service（未来的
// FS 等）bind 消费。契约只在本 SDK 定义（provider 是驱动组件，KIND = Device）；
// Core 不认识该接口语义，只存 api/ctx 指针 + exact ABI。

// 声明本体（`#[repr(C)]` function table + 名字 / 指纹 / sector 常量）由
// `tools/kabi/kabi_gen.py` 从 `abi/block.toml` 生成到 [`crate::generated::block`]：
// 布局断言（`const _`）与 C 侧 `_Static_assert` 同源，`make abi-check` 保证
// 生成物与 schema 不漂移。本文件只保留语义 facade 并 re-export 既有路径。

/// `block.device` 接口的稳定名字（publish / bind 必须逐字节一致）。
pub use crate::generated::block::KCOMP_BLOCK_DEVICE_NAME as BLOCK_DEVICE_NAME;

use crate::generated::block::{KCOMP_BLOCK_DEVICE_ABI, KCOMP_BLOCK_DEVICE_CONTRACT};

/// BlockDevice 的 `#[repr(C)]` function table（provider/consumer 共享布局）。
///
/// 定义与布局断言在生成物 [`crate::generated::block`]（schema = `abi/block.toml`）；
/// 这里 re-export 以保持 `block::BlockDeviceApi` 路径。
pub use crate::generated::block::BlockDeviceApi;

/// `block.device` 契约（KIND = Device）。
pub struct BlockDevice;

/// Endpoint 模型的契约身份（contract id + exact ABI + 领域分类）。
impl Contract for BlockDevice {
    const ID: u64 = KCOMP_BLOCK_DEVICE_CONTRACT;
    const ABI: u64 = KCOMP_BLOCK_DEVICE_ABI;
    const KIND: InterfaceKind = InterfaceKind::Device;
}

// -----------------------------------------------------------------------
// provider wrapper：纯 Rust 实现 → SDK 生成 `#[repr(C)]` table
// -----------------------------------------------------------------------

/// 契约单位：1 sector = 512 字节（wrapper 校验入参用；不是对外 API）。
const SECTOR_SIZE: usize = crate::generated::block::KCOMP_BLOCK_DEVICE_SECTOR;

/// block 设备的 provider 接口：驱动实现它，ABI table 由 SDK 生成。
///
/// 纯 Rust：无 `unsafe` / 无 `extern "C"` / 无裸指针。`Err` 侧是 `-Errno` 形式
/// （与 Core 导出、[`BlockDeviceApi`] 的返回约定一致；SDK 原样透传，不取反）。
///
/// 实现者仍受 [`BlockDeviceApi`] 契约约束：阻塞到本次传输完成、只在 task 上下文
/// 调用、`buf` 指向 Core 可见 RAM（v1 无 IOMMU：物理地址 == 虚拟地址）。
pub trait BlockDeviceProvider {
    /// 设备容量（单位：512 字节 sector）。
    fn capacity_sectors(&self) -> u64;

    /// 从 `lba` 读 `buf.len()` 字节到 `buf`。
    ///
    /// adapter 已保证 `buf.len() > 0` 且是 512 的整数倍；不满足时调用方拿到
    /// `-EINVAL`，本方法**不会被调用**。
    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()>;

    /// 从 `buf` 写 `buf.len()` 字节到 `lba`（入参保证同 [`BlockDeviceProvider::read`]）。
    fn write(&self, lba: u64, buf: &[u8]) -> Result<()>;
}

/// provider 实现与生成的 `#[repr(C)]` table 的配对；放进 `static` 后
/// [`publish`](Self::publish)。
///
/// `new` 是 `const fn`，可直接做 `static` 初始化：
///
/// ```text
/// static DEVICE: BlockDeviceService<VirtioBlkDevice> =
///     BlockDeviceService::new(VirtioBlkDevice { ... });
/// ```
pub struct BlockDeviceService<P: BlockDeviceProvider> {
    provider: P,
    api: BlockDeviceApi,
}

impl<P: BlockDeviceProvider> BlockDeviceService<P> {
    /// 由 `P` 生成 `#[repr(C)]` table：三个字段分别指向 `P` 的**单态化** adapter。
    ///
    /// `const fn`（static 地址稳定，见 [`BlockDeviceService::publish`] 与
    /// [`BlockDeviceService::ctx`]）。
    pub const fn new(provider: P) -> Self {
        Self {
            provider,
            api: BlockDeviceApi {
                capacity_sectors: capacity_sectors::<P>,
                read: read::<P>,
                write: write::<P>,
            },
        }
    }

    /// 发布 `block.device` **endpoint**（staged：只在 `kcomp_instance_create`
    /// 期间有效；Core 在 create 返回 0 后原子提交）。
    ///
    /// `port_name` 是组合策略分配的端点名（在 provider 实例内唯一）；`port` 是
    /// provider 定义的不透明 dispatch token——**Gate** 路径经 image 的
    /// `kcomp_service_dispatch` 用它选中本契约。发布同时交付 **Direct** 的
    /// `api` / `ctx`；**机制由 Core 在 bind 时按两端执行域选定**，provider 两种
    /// transport 都提供、**不选择**（`docs/architecture/deployment.md` §2）。
    ///
    /// # 为什么这是安全 fn
    ///
    /// `kcore_endpoint_publish` 是 `unsafe` extern：调用方可以递出一张与契约布局
    /// 不匹配的 table。这里不成立：`self.api` 不是外部数据，而是
    /// [`BlockDeviceService::new`] 从 `P` 生成的值（布局就是 `BlockDeviceApi` 类型
    /// 本身）；`ctx` 是 `'static` 实例里 provider 字段的地址（[`BlockDeviceService::ctx`]），
    /// 在 `'static` 内不会失效。两个 unsafe 前提都在本模块闭环，provider 作者因此
    /// 永远不写 unsafe。
    pub fn publish_endpoint(&'static self, port_name: &[u8], port: u32) -> Result<()> {
        // SAFETY: api 布局 = BlockDeviceApi（new 从 P 生成）；ctx = &'static
        // self.provider（地址稳定）；Core 只存指针、不解引用。
        let status = unsafe {
            crate::abi::kcore_endpoint_publish(
                port_name.as_ptr(),
                port_name.len(),
                <BlockDevice as Contract>::ID,
                <BlockDevice as Contract>::KIND.as_u32(),
                <BlockDevice as Contract>::ABI,
                port,
                (&self.api as *const BlockDeviceApi).cast(),
                self.ctx(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(Errno::from_code(status))
        }
    }

    /// provider 的 opaque `ctx` = `&self.provider`（Core 原样回传给 table 方法）。
    ///
    /// 地址稳定：`&'static self` 只可能来自 `static`（或泄漏的 `'static` 分配），
    /// 该内存此后不移动，provider 字段随之固定。每个 `BlockDeviceService<P>` 实例
    /// 有自己的代码（单态化 adapter）和数据（`provider` + `api` 表），所以 `ctx`
    /// 只需指回 provider 本身，**不需要**携带类型标签 / 实例 id——table 与 ctx
    /// 合起来就是实例身份。测试 / 诊断用；正常 provider 不碰它。
    pub fn ctx(&'static self) -> *mut () {
        core::ptr::addr_of!(self.provider).cast_mut().cast()
    }

    /// 生成的 `#[repr(C)]` table（测试 / 诊断用；Core 拿到的就是它）。
    pub fn api(&self) -> &BlockDeviceApi {
        &self.api
    }
}

// -----------------------------------------------------------------------
// adapter：`BlockDeviceApi` 字段的实际函数（按 `::<P>` 单态化）
// -----------------------------------------------------------------------
//
// 必须是**模块层** fn：嵌套 fn 无法引用外层泛型参数，adapter 只能在这里定义、
// 在 new 里以 `::<P>` 实例化成 table 里的非泛型函数指针。

/// `BlockDeviceApi::capacity_sectors` 的 adapter。
///
/// # Safety
/// `ctx` 必须是 [`BlockDeviceService::publish`] 交付的 `&'static P` —— 本模块是
/// 该指针的唯一构造者，Core 只按 endpoint 记录原样回传。
unsafe extern "C" fn capacity_sectors<P: BlockDeviceProvider>(ctx: *mut ()) -> u64 {
    // SAFETY: 见 Safety；ctx 恒为有效的 &'static P。
    let provider = unsafe { &*ctx.cast::<P>() };
    provider.capacity_sectors()
}

/// `BlockDeviceApi::read` 的 adapter：统一校验入参、构造 slice、调 `P::read`。
///
/// # Safety
/// 同 [`capacity_sectors`]；此外调用方保证 `buf` 在调用期间有效，且按契约指向
/// Core 可见 RAM。
unsafe extern "C" fn read<P: BlockDeviceProvider>(
    ctx: *mut (),
    lba: u64,
    buf: *mut u8,
    len: usize,
) -> i32 {
    // 契约入参在这里统一校验一次（provider 不重复校验）：
    // null / 空 / 非 512 整数倍 → -EINVAL。
    if buf.is_null() || len == 0 || !len.is_multiple_of(SECTOR_SIZE) {
        return Errno::EINVAL.code();
    }
    // SAFETY: ctx 由本模块生成（= &'static P，见 capacity_sectors 的 Safety）；
    // buf 非空、len 为 512 的整数倍，且按契约在调用期间位于 Core 可见 RAM、独占
    // 可写（v1 无 IOMMU：设备地址 == 物理地址 == 虚拟地址）。slice 只在 provider
    // 调用期间存活，不逃逸。
    let provider = unsafe { &*ctx.cast::<P>() };
    let buf = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    match provider.read(lba, buf) {
        Ok(()) => 0,
        Err(errno) => errno.code(),
    }
}

/// `BlockDeviceApi::write` 的 adapter（校验与映射同 [`read`]）。
///
/// # Safety
/// 同 [`read`]。
unsafe extern "C" fn write<P: BlockDeviceProvider>(
    ctx: *mut (),
    lba: u64,
    buf: *const u8,
    len: usize,
) -> i32 {
    if buf.is_null() || len == 0 || !len.is_multiple_of(SECTOR_SIZE) {
        return Errno::EINVAL.code();
    }
    // SAFETY: 同 read 的 slice 构造；const 侧只读。
    let provider = unsafe { &*ctx.cast::<P>() };
    let buf = unsafe { core::slice::from_raw_parts(buf, len) };
    match provider.write(lba, buf) {
        Ok(()) => 0,
        Err(errno) => errno.code(),
    }
}
