//! Component → Component Service binding 的薄 SDK 层。
//!
//! 这一层把 ABI 编码（kind / fingerprint）变成类型、把 `0/-errno` 变成
//! `Result`、把裸指针收窄成 [`RawBinding`] / [`ServiceBinding`]。
//! 契约由实现 [`Service`] 的类型表达（provider 与 consumer 共享），本模块
//! 随附 `SchedulerPolicy` / `DriverProber` 两个契约；`BlockDevice` 的契约与
//! provider wrapper 在 [`crate::block`]（这里只 re-export）。
//! **不做**字符串函数查找 / 反射 /
//! 动态类型——KernelNative phase 1 就是 typed `#[repr(C)]` function table + direct call。

use crate::abi;
use crate::errno::{Errno, Result};

/// Exact ABI fingerprint（`#[repr(transparent)] u64`，**无版本兼容语义**）。
/// 与 Core `component::interface::InterfaceAbi` 镜像。
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceAbi(u64);

impl InterfaceAbi {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Interface 领域分类（ABI 编码 0/1/2，与 Core `InterfaceKind` 一致）。
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceKind {
    Device = 0,
    Service = 1,
    Policy = 2,
}

impl InterfaceKind {
    /// ABI 编码（`#[repr(u32)]`，恒等于判别值）。
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// 裸 binding 快照（Core 交付 `api` / `ctx` / `generation` + 稳定 `BindingId`）。
///
/// 所有类型化 binding 的底座；untyped / Core 侧风格直接使用它。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawBinding {
    /// 稳定逻辑 binding 身份；provider replacement 不改变它。
    pub binding: u64,
    /// provider `#[repr(C)]` function table 指针（opaque）。
    pub api: usize,
    /// provider opaque state/context。
    pub ctx: usize,
    /// provider commit 计数。
    pub generation: u64,
}

/// 一个 Service 契约：provider 与 consumer 共享。
///
/// 契约由**一个类型**表达；provider 用 [`publish_service`] 发布，consumer 用
/// [`ServiceBinding`] 绑定。**不定义具体行为**——`Api` 是 provider 的
/// `#[repr(C)]` function table，KernelNative phase 1 就是 typed table + direct call，
/// 没有字符串查找 / 反射 / 动态类型。
pub trait Service {
    /// 接口稳定名字（publish / bind 必须逐字节一致）。
    const NAME: &'static [u8];
    /// 接口领域分类。
    const KIND: InterfaceKind;
    /// exact ABI fingerprint。
    const ABI: InterfaceAbi;
    /// provider 的 `#[repr(C)]` function table。
    type Api: Copy;
}

/// 类型化 binding：Core 校验过的 [`RawBinding`] + 契约类型 `S`。
///
/// `api` 指向 provider 的 `#[repr(C)]` function table（布局 = `S::Api`）；
/// Core 在 bind / refresh 时已校验 provider Ready 且 exact ABI 一致。
pub struct ServiceBinding<S: Service> {
    binding: u64,
    api: *const S::Api,
    ctx: *mut (),
    generation: u64,
}

impl<S: Service> Clone for ServiceBinding<S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: Service> Copy for ServiceBinding<S> {}

impl<S: Service> ServiceBinding<S> {
    /// 按 `S` 的契约绑定（Core exact-compare ABI + 验证 provider）。
    pub fn bind() -> Result<Self> {
        Ok(Self::from_raw(bind(S::NAME, S::KIND, S::ABI)?))
    }

    /// 用已有 binding id refresh：Core 重新验证 provider 后返回最新快照。
    pub fn refresh(&mut self) -> Result<()> {
        let raw = refresh(self.binding, S::ABI)?;
        self.api = raw.api as *const S::Api;
        self.ctx = raw.ctx as *mut ();
        self.generation = raw.generation;
        Ok(())
    }

    /// provider function table。Core 已校验 provider + exact ABI，布局即 `S::Api`。
    pub fn api(&self) -> &S::Api {
        // SAFETY: Core 在 bind / refresh 时已校验 provider Ready 且 ABI fingerprint
        // 完全一致；`api` 指向 provider 'static 的 #[repr(C)] function table。
        unsafe { &*self.api }
    }

    /// provider opaque state/context（原样回传给 `S::Api` 的方法）。
    pub fn ctx(&self) -> *mut () {
        self.ctx
    }

    /// provider commit 计数。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn from_raw(raw: RawBinding) -> Self {
        Self {
            binding: raw.binding,
            api: raw.api as *const S::Api,
            ctx: raw.ctx as *mut (),
            generation: raw.generation,
        }
    }
}

/// SchedulerPolicy 的 exact ABI fingerprint。
///
/// 必须与 Core `sched::SCHEDULER_POLICY_ABI` 完全一致（A/B 双侧手工锚定，
/// 两侧各有锚定测试钉死数值）。
pub const SCHEDULER_POLICY_ABI: InterfaceAbi = InterfaceAbi::from_raw(0x5343_4845_4455_4C52);

/// 发布接口（**只在 `kcomp_instance_create` 期间有效**）：Core 记录 pending，
/// `kcomp_instance_create` 返回 0 后原子提交。返回 `Ok(())` = 已记录 pending。
///
/// # Safety
/// `api` 必须指向 `'static` 的 `#[repr(C)]` function table，`ctx` 必须是
/// provider 存活期内有效的 opaque state；Core 只存指针、不解引用。
pub unsafe fn publish(
    name: &[u8],
    kind: InterfaceKind,
    abi: InterfaceAbi,
    api: *const (),
    ctx: *mut (),
) -> Result<()> {
    // SAFETY: 调用方保证 api/ctx 契约（见函数 Safety）。
    let status = unsafe {
        abi::kcore_interface_publish(
            name.as_ptr(),
            name.len(),
            kind.as_u32(),
            abi.raw(),
            api,
            ctx,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(Errno::from_code(status))
    }
}

/// 类型化 publish：provider 发布 `S` 的 `#[repr(C)]` function table。
///
/// # Safety
/// 同 [`publish`]：`api` 必须指向 `'static` 且布局 = `S::Api` 的 function table，
/// `ctx` 必须是 provider 存活期内有效的 opaque state。
pub unsafe fn publish_service<S: Service>(api: *const S::Api, ctx: *mut ()) -> Result<()> {
    // SAFETY: 调用方保证 `api` 布局 = `S::Api`（见 Safety）。
    unsafe { publish(S::NAME, S::KIND, S::ABI, api as *const (), ctx) }
}

/// 类型化 publish，endpoint 名由调用方给定（ABI 指纹仍是 `S::ABI`）。
///
/// 多实例场景下同一契约的每个实例发布到不同 endpoint（组合策略分配名字）；
/// 单例角色继续用固定名 helper [`publish_service`]。Core 语义不变（staged、
/// exact ABI、`0/-errno`）。
///
/// # Safety
/// 同 [`publish`]：`api` 必须指向 `'static` 且布局 = `S::Api` 的 function table，
/// `ctx` 必须是 provider 存活期内有效的 opaque state。
pub unsafe fn publish_named<S: Service>(
    name: &[u8],
    api: *const S::Api,
    ctx: *mut (),
) -> Result<()> {
    // SAFETY: 调用方保证 `api` 布局 = `S::Api`（见 Safety）。
    unsafe { publish(name, S::KIND, S::ABI, api as *const (), ctx) }
}

/// consumer 按名 bind：Core exact-compare ABI + 验证 provider 后返回当前快照。
pub fn bind(name: &[u8], kind: InterfaceKind, abi: InterfaceAbi) -> Result<RawBinding> {
    let (mut binding, mut api, mut ctx, mut generation) = (0u64, 0usize, 0usize, 0u64);
    // SAFETY: (name_ptr, len) 与四个 out 在本帧内有效；Core 写入 out。
    let status = unsafe {
        abi::kcore_interface_bind(
            name.as_ptr(),
            name.len(),
            kind.as_u32(),
            abi.raw(),
            &mut binding,
            &mut api,
            &mut ctx,
            &mut generation,
        )
    };
    if status == 0 {
        Ok(RawBinding {
            binding,
            api,
            ctx,
            generation,
        })
    } else {
        Err(Errno::from_code(status))
    }
}

/// 类型化 bind，endpoint 名由调用方给定（Core 仍 exact-compare `S::ABI`）。
///
/// 与 [`ServiceBinding::bind`] 的唯一区别是名字：单例角色用固定 `S::NAME`，
/// 多实例场景用组合策略分配的 endpoint 名（provider 侧对应 [`publish_named`]）。
pub fn bind_named<S: Service>(name: &[u8]) -> Result<ServiceBinding<S>> {
    Ok(ServiceBinding::from_raw(bind(name, S::KIND, S::ABI)?))
}

/// consumer 用已有 binding id refresh：Core exact-compare ABI + 重新验证
/// provider 后返回最新快照（provider replacement 后无需 ELF reload）。
pub fn refresh(binding: u64, abi: InterfaceAbi) -> Result<RawBinding> {
    let (mut api, mut ctx, mut generation) = (0usize, 0usize, 0u64);
    // SAFETY: 三个 out 在本帧内有效；Core 写入 out。
    let status = unsafe {
        abi::kcore_interface_refresh(binding, abi.raw(), &mut api, &mut ctx, &mut generation)
    };
    if status == 0 {
        Ok(RawBinding {
            binding,
            api,
            ctx,
            generation,
        })
    } else {
        Err(Errno::from_code(status))
    }
}

/// 只读查询 `(name, kind, abi)` 是否已绑定且 provider 存活。
pub fn available(name: &[u8], kind: InterfaceKind, abi: InterfaceAbi) -> bool {
    // SAFETY: (name_ptr, len) 在本帧内有效；Core 只读。
    unsafe {
        abi::kcore_interface_available(name.as_ptr(), name.len(), kind.as_u32(), abi.raw()) == 1
    }
}

// -----------------------------------------------------------------------
// scheduler —— SchedulerPolicy 契约（provider: scheduler_rr 等策略组件）
// -----------------------------------------------------------------------

/// `scheduler` 接口名字（publish / bind 必须逐字节一致）。
pub const SCHEDULER_POLICY_NAME: &[u8] = b"scheduler";

/// SchedulerPolicy 的 `#[repr(C)]` function table（provider/consumer 共享布局）。
///
/// `ctx` 由 Core 从 binding 单独取出后原样传入 `choose_next`，不在 table 内。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SchedulerPolicyApi {
    /// 提议下一个 TaskId；Core 会验证它落在传入的 runnable 列表内。
    pub choose_next:
        extern "C" fn(ctx: *mut (), runnable: *const u32, count: usize, current: u32) -> u32,
}

/// SchedulerPolicy 契约（KIND = Policy）。
pub struct SchedulerPolicy;

impl Service for SchedulerPolicy {
    const NAME: &'static [u8] = SCHEDULER_POLICY_NAME;
    const KIND: InterfaceKind = InterfaceKind::Policy;
    const ABI: InterfaceAbi = SCHEDULER_POLICY_ABI;
    type Api = SchedulerPolicyApi;
}

// -----------------------------------------------------------------------
// driver.prober —— 设备 prober → 驱动 的**分配（assignment）**接口
// -----------------------------------------------------------------------
//
// 这是 prober→driver 的**唯一**通道，只传**数据**（device id / attempt），
// 绝不携带 authority：prober 不 claim MMIO、不读任何寄存器；驱动在**自己的
// init 上下文**里 `kcore_mmio_claim` 那个 DeviceId，并自己做协议级 fine match
// （见 docs/driver-model.md §9.1 / §12 Q1）。
//
// 分工不可合并：coarse candidate match（prober，只认 compatible 这个 opaque
// 键）→ 请求 Core 加载候选驱动代码 → fine protocol match（driver，需要协议知识
// + authority 上下文）。在驱动代码跑起来之前不可能完成最后的硬件匹配——这是
// 刻意的分层，不是含糊。

/// `driver.prober` 接口的稳定名字（publish / bind 必须逐字节一致）。
pub const DRIVER_PROBER_NAME: &[u8] = b"driver.prober";

/// `driver.prober` 的 exact ABI fingerprint（ASCII "DRVPROBE"）。
///
/// prober 与 driver 都是组件；契约只在本 SDK 定义（Core 不认识该接口语义）。
/// 两侧各有锚定测试钉死数值，防止漂移。
pub const DRIVER_PROBER_ABI: InterfaceAbi = InterfaceAbi::from_raw(0x4452_5650_524F_4245);

/// `report_attempt` 的 outcome 编码（**Component ABI**，改动 = 破坏性变更）。
///
/// ```text
///  0  Match    —— 驱动接受该设备（claim + fine protocol match 均通过）
///  1  NoMatch  —— 驱动检查后拒绝（协议身份不符），已 release claim
/// <0  Error    —— 驱动无法完成检查（value = -Errno）；detail 放观测值或 0
/// ```
pub const ASSIGN_MATCH: i32 = 0;
/// 见 [`ASSIGN_MATCH`] 的 outcome 编码说明。
pub const ASSIGN_NO_MATCH: i32 = 1;

/// `driver.prober` 的 `#[repr(C)]` function table（provider/consumer 共享布局）。
///
/// 两个回调**只读写 prober 自己的记录**：不 claim / release 任何资源，不替驱动
/// 创建任务。`ctx` 是 prober 的 opaque state（由 Core 从 binding 取出后原样回传）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DriverProberApi {
    /// 取该驱动声明的 compatible 对应的**下一台**候选设备（prober-owned cursor）。
    ///
    /// 成功 = 0 且写入 `attempt`（prober 分配的序号，仅用于拒绝 stale report，
    /// **不是 capability**）与 `device_id`（identity）；无更多 = `-ENOENT`。
    pub next_assignment: extern "C" fn(
        ctx: *mut (),
        driver_name: *const u8,
        driver_name_len: usize,
        out_attempt: *mut u32,
        out_device_id: *mut u32,
    ) -> i32,
    /// 记录驱动对某个 `attempt` 的结果（编码见 [`ASSIGN_MATCH`]；`detail` 由驱动定义）。
    /// 未知 / 尚未下发 / 重复上报的 attempt 一律拒绝。
    pub report_attempt: extern "C" fn(ctx: *mut (), attempt: u32, outcome: i32, detail: u32) -> i32,
}

/// `driver.prober` 契约（KIND = Service）。
///
/// consumer 绑定后经 `api()` 调用 `next_assignment` / `report_attempt`；接口只传
/// **数据**——驱动仍须自己 `kcore_mmio_claim`，并在自己上下文里 revalidate 协议身份。
pub struct DriverProber;

impl Service for DriverProber {
    const NAME: &'static [u8] = DRIVER_PROBER_NAME;
    const KIND: InterfaceKind = InterfaceKind::Service;
    const ABI: InterfaceAbi = DRIVER_PROBER_ABI;
    type Api = DriverProberApi;
}

// -----------------------------------------------------------------------
// block.device —— 契约与 provider wrapper 同处一室：[`crate::block`]
// -----------------------------------------------------------------------
//
// 定义（function table / 契约类型 / BlockDeviceProvider / BlockDeviceService）
// 都在 `crate::block`；这里只 re-export，让既有路径 `binding::BlockDevice` /
// `binding::BlockDeviceApi` 保持不变。

pub use crate::block::{BLOCK_DEVICE_ABI, BLOCK_DEVICE_NAME, BlockDevice, BlockDeviceApi};
pub use crate::filesystem::{
    FileSystem,
    FileSystemApi,
    FILESYSTEM_ABI,
    FILESYSTEM_NAME,
    FILESYSTEM_OPEN_READ,
};
