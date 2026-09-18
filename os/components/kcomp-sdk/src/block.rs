//! `block.device` 契约（KIND = Device）+ provider 侧 ergonomic wrapper。
//!
//! 本模块是 block 契约的**唯一**住处：`#[repr(C)]` function table
//! （[`BlockDeviceApi`]）、契约类型（[`BlockDevice`]）与 provider 包装
//! （[`BlockDeviceProvider`] / [`BlockDeviceService`]）放在一起；[`crate::binding`]
//! 只 re-export，保持 `binding::BlockDevice` 等既有路径不变。
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
//! DEVICE.publish()?;                              // 安全 fn；Core 在 instance_create 返回后 commit
//! ```
//!
//! [`BlockDeviceService::new`] 用单态化 adapter 从 `P` 生成 table；adapter 统一执行
//! 契约的入参校验（null / 空 / 非 512 倍数 → `-EINVAL`）并把裸指针收窄成
//! `&[u8]` / `&mut [u8]`；[`BlockDeviceService::publish`] 的安全性论证见其文档。

use crate::binding::{InterfaceAbi, InterfaceKind, Service, publish_service};

// -----------------------------------------------------------------------
// 契约：block.device —— 驱动提供的 Device Interface（provider: virtio_blk）
// -----------------------------------------------------------------------
//
// docs/driver-model.md §9.1 ⑤：驱动 claim 完 MmioHandle / IrqHandle / DmaHandle
// 后向 Component Interface Registry provides 本接口，供上层 Service（未来的
// FS 等）bind 消费。契约只在本 SDK 定义（provider 是驱动组件，KIND = Device）；
// Core 不认识该接口语义，只存 api/ctx 指针 + exact ABI，与 `driver.prober` 同类。

/// `block.device` 接口的稳定名字（publish / bind 必须逐字节一致）。
pub const BLOCK_DEVICE_NAME: &[u8] = b"block.device";

/// `block.device` 的 exact ABI fingerprint。
///
/// 数值 = 8 字节 ASCII tag `b"BLOCKDEV"` 的大端读数
/// （`0x424C_4F43_4B44_4556`，可直接按字节读出拼写——与 `SCHEDULER_POLICY_ABI`
/// 同一约定）。provider / consumer 都由本 SDK 的同一份定义编译；锚定测试把数值
/// 钉死，任何改动必须是一次刻意的测试修改（数值漂移 = Core 直接拒绝 bind）。
pub const BLOCK_DEVICE_ABI: InterfaceAbi = InterfaceAbi::from_raw(0x424C_4F43_4B44_4556);

/// BlockDevice 的 `#[repr(C)]` function table（provider/consumer 共享布局）。
///
/// # 契约（两个实现能否互通，全看这几条）
///
/// - **单位**：`lba` 以 **512 字节 sector** 计；`len` 是**字节数**，必须是
///   sector 大小的整数倍。
/// - **同步 / 阻塞**：`read` / `write` 阻塞到本次传输完成。当前实现（virtio_blk）
///   轮询设备，因此调用方**不得**处于不能阻塞的上下文。
/// - **调用上下文**：只在 **task 上下文**调用；禁止 trap / 中断上下文。
/// - **返回约定**：`0` = 成功，`-Errno` = 失败（与 `kcore_*` 导出一致）。
/// - **非法参数**：`buf` 为 null / `len == 0` / `len` 非 512 的整数倍 → `-EINVAL`。
///   用 [`BlockDeviceService`] 发布的 provider 由 SDK 统一挡下（provider 不会被
///   调用）；手写 table 的 provider 需自行保证同语义。
/// - **buffer（临时契约）**：`buf` 是裸指针，实现要求它指向 **Core 可见 RAM**
///   （v1 无 IOMMU：设备地址 == 物理地址 == 虚拟地址）。
///
/// # `buf` 裸指针是刻意的临时选择（不是最终设计）
///
/// 按 AGENTS.md，"裸指针只来自 Core 派生并持有 provenance 的 typed Lease"——
/// 裸指针本身不是 authority。这里直接收裸指针，是因为 consumer 侧"分配
/// DMA-able 内存"的窄接口**尚不存在**，v1 只有这一种可行形状。**一旦出现
/// 跨执行域的调用方，该参数必须换成 Core 派生的 DMA lease / handle**（那时
/// 裸地址不再能证明 buffer 归属与设备可达性）；在那之前它只是暂时够用。
///
/// 契约刻意保持最小：只有 capacity_sectors / read / write；flush / sector_size /
/// ioctl 等不在本轮，等真实需求（如 FS 落盘屏障）出现再定。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BlockDeviceApi {
    /// 设备容量（单位：512 字节 sector）。
    pub capacity_sectors: unsafe extern "C" fn(ctx: *mut ()) -> u64,
    /// 从 `lba` 读 `len` 字节到 `buf`（单位 / 阻塞 / 上下文见契约文档）。
    pub read: unsafe extern "C" fn(ctx: *mut (), lba: u64, buf: *mut u8, len: usize) -> i32,
    /// 从 `buf` 写 `len` 字节到 `lba`（单位 / 阻塞 / 上下文见契约文档）。
    pub write: unsafe extern "C" fn(ctx: *mut (), lba: u64, buf: *const u8, len: usize) -> i32,
}

/// `block.device` 契约（KIND = Device）。
pub struct BlockDevice;

impl Service for BlockDevice {
    const NAME: &'static [u8] = BLOCK_DEVICE_NAME;
    const KIND: InterfaceKind = InterfaceKind::Device;
    const ABI: InterfaceAbi = BLOCK_DEVICE_ABI;
    type Api = BlockDeviceApi;
}

// -----------------------------------------------------------------------
// provider wrapper：纯 Rust 实现 → SDK 生成 `#[repr(C)]` table
// -----------------------------------------------------------------------

/// 契约单位：1 sector = 512 字节（wrapper 校验入参用；不是对外 API）。
const SECTOR_SIZE: usize = 512;

/// Core errno 约定（`0` / `-errno`；`EINVAL = 22`，见 os/core/src/errno.rs）。
const EINVAL: i32 = -22;

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
    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), i32>;

    /// 从 `buf` 写 `buf.len()` 字节到 `lba`（入参保证同 [`BlockDeviceProvider::read`]）。
    fn write(&self, lba: u64, buf: &[u8]) -> Result<(), i32>;
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

    /// 发布 `block.device`（staged：只在 `kcomp_instance_create` 期间有效，
    /// Core 在 `kcomp_instance_create` 返回 0 后原子提交）。
    ///
    /// # 为什么这是安全 fn
    ///
    /// [`publish_service`] 带 `unsafe`，是因为**它的调用方**可以递出一张与
    /// `S::Api` 布局不匹配的 table——那个不变量它无法自证。这里不成立：
    /// `self.api` 不是外部数据，而是 [`BlockDeviceService::new`] 从 `P` 生成的值
    /// （布局就是 `BlockDeviceApi` 类型本身）；`ctx` 是 `'static` 实例里 provider
    /// 字段的地址（[`BlockDeviceService::ctx`]），在 `'static` 内不会失效。
    /// 两个 unsafe 前提都在本模块闭环，provider 作者因此永远不写 unsafe。
    pub fn publish(&'static self) -> Result<(), i32> {
        // SAFETY: api 由 new 从 P 原地生成（布局 = BlockDeviceApi，不是调用方数据）；
        // ctx = &'static self.provider（地址稳定性见 ctx()）。Core 只存指针、不解引用。
        unsafe { publish_service::<BlockDevice>(&self.api, self.ctx()) }
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
/// 该指针的唯一构造者，Core 只按 binding 原样回传。
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
        return EINVAL;
    }
    // SAFETY: ctx 由本模块生成（= &'static P，见 capacity_sectors 的 Safety）；
    // buf 非空、len 为 512 的整数倍，且按契约在调用期间位于 Core 可见 RAM、独占
    // 可写（v1 无 IOMMU：设备地址 == 物理地址 == 虚拟地址）。slice 只在 provider
    // 调用期间存活，不逃逸。
    let provider = unsafe { &*ctx.cast::<P>() };
    let buf = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    match provider.read(lba, buf) {
        Ok(()) => 0,
        Err(errno) => errno,
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
        return EINVAL;
    }
    // SAFETY: 同 read 的 slice 构造；const 侧只读。
    let provider = unsafe { &*ctx.cast::<P>() };
    let buf = unsafe { core::slice::from_raw_parts(buf, len) };
    match provider.write(lba, buf) {
        Ok(()) => 0,
        Err(errno) => errno,
    }
}
