//! kcomp-sdk —— 组件 SDK / CRT（step 2）。
//!
//! 这个 crate 解决三件互相独立的事，全部**随 `.kcomp` 私有携带**（不建 shared
//! Rust runtime，见 docs/component-model.md §2.2）：
//!
//! 1. [`abi`]：`kcore_*` 导出白名单的**单一来源**（组件不再各自复制 extern 块）；
//! 2. 入口 / 日志 / panic adapter：`kcomp_init!`、[`log`]/`klog!`、[`panic_handler!`]；
//! 3. 可选的 alloc adapter（feature `alloc`）：`GlobalAlloc` → Core 共享堆。
//!
//! # panic adapter（本 crate 存在的关键理由）
//!
//! 组件以链接后的 ET_REL 加载，镜像里带自己的 `#[panic_handler]`——组件 `panic!`
//! 时进入的是这里，而不是 boot 镜像的 panic handler。adapter 只做两件事：
//! 经 [`abi::kcore_log_line`] 打印一行诊断，然后调 [`abi::kcore_panic_escape`]
//! 把控制权交还 Core（活动 containment 边界内它**永不返回**，见
//! `component/containment.rs`）。若没有活动边界（返回 `-EPERM`），说明这次 panic
//! 不在任何组件边界内，只能停在原地自旋（安全失败）。
//!
//! # alloc adapter
//!
//! `#[global_allocator]` 不是"每个组件自带堆"：它只是把 Rust `GlobalAlloc`
//! 契约接到 **Core 共享堆**（`kcore_heap_alloc/dealloc`）。默认关闭，组件按需
//! 通过 `kcomp-sdk = { path = "...", features = ["alloc"] }` 开启。

#![no_std]

// host 测试用（`cargo test`）；裸机目标不编入。
#[cfg(test)]
extern crate std;

/// `kcore_*` 导出 ABI（EXPORT_SYMBOL 教学版）。
///
/// 声明即契约：名字必须与 Core `component/export.rs` 的白名单逐字节一致，签名
/// 错误 = UB（loader 只按名字精确解析，不校验签名）。这里保持**全量**声明，
/// 让各组件只共用这一份；新增 Core 导出时同步加在这里。
pub mod abi {
    /// IRQ 投递回调：`ctx` 原样回传，Core 不解引用。
    pub type IrqHandler = extern "C" fn(ctx: *mut ());

    // 安全说明：以下符号由 Core 保证实现；调用方必须满足各自契约（指针有效性、
    // out 参数可写、handle 归宿等），故调用点均为 `unsafe`。
    unsafe extern "C" {
        // -- Runtime / shared heap（Core 共享堆，非 per-component 堆）--
        #[link_name = "kcore_heap_alloc"]
        pub fn kcore_heap_alloc(size: usize, align: usize) -> *mut u8;
        #[link_name = "kcore_heap_dealloc"]
        pub fn kcore_heap_dealloc(ptr: *mut u8, size: usize, align: usize) -> i32;

        // -- Logging / diagnostics --
        #[link_name = "kcore_console_write_byte"]
        pub fn kcore_console_write_byte(byte: u8);
        #[link_name = "kcore_log_line"]
        pub fn kcore_log_line(ptr: *const u8, len: usize) -> i32;

        // -- Machine query --
        #[link_name = "kcore_machine_boot_hart"]
        pub fn kcore_machine_boot_hart() -> u32;
        #[link_name = "kcore_machine_cpu_count"]
        pub fn kcore_machine_cpu_count() -> u32;
        #[link_name = "kcore_machine_has_hart"]
        pub fn kcore_machine_has_hart(hart_id: u32) -> i32;

        // -- System query --
        #[link_name = "kcore_free_page_count"]
        pub fn kcore_free_page_count() -> u32;
        #[link_name = "kcore_task_count"]
        pub fn kcore_task_count() -> u32;
        #[link_name = "kcore_component_count"]
        pub fn kcore_component_count() -> u32;

        // -- Component lifecycle --
        #[link_name = "kcore_component_load"]
        pub fn kcore_component_load(name: *const u8, len: usize) -> i32;
        #[link_name = "kcore_interface_publish"]
        pub fn kcore_interface_publish(
            name: *const u8,
            len: usize,
            kind: u32,
            abi: u64,
            api: *const (),
            ctx: *mut (),
        ) -> i32;
        #[link_name = "kcore_interface_available"]
        pub fn kcore_interface_available(name: *const u8, len: usize, kind: u32, abi: u64) -> i32;
        #[link_name = "kcore_interface_bind"]
        pub fn kcore_interface_bind(
            name: *const u8,
            len: usize,
            kind: u32,
            abi: u64,
            out_binding: *mut u64,
            out_api: *mut usize,
            out_ctx: *mut usize,
            out_generation: *mut u64,
        ) -> i32;
        #[link_name = "kcore_interface_refresh"]
        pub fn kcore_interface_refresh(
            binding: u64,
            abi: u64,
            out_api: *mut usize,
            out_ctx: *mut usize,
            out_generation: *mut u64,
        ) -> i32;

        // -- Task control --
        #[link_name = "kcore_task_create"]
        pub fn kcore_task_create(entry: usize) -> i32;
        #[link_name = "kcore_task_start"]
        pub fn kcore_task_start(id: u32) -> i32;
        #[link_name = "kcore_task_yield"]
        pub fn kcore_task_yield() -> i32;
        #[link_name = "kcore_task_exit"]
        pub fn kcore_task_exit() -> i32;
        #[link_name = "kcore_task_state"]
        pub fn kcore_task_state(id: u32) -> i32;

        // -- Panic containment --
        #[link_name = "kcore_panic_escape"]
        pub fn kcore_panic_escape() -> i32;

        // -- Scheduler --
        #[link_name = "kcore_sched_run"]
        pub fn kcore_sched_run() -> i32;

        // -- Resource authority: device discovery / MMIO --
        #[link_name = "kcore_device_nth"]
        pub fn kcore_device_nth(
            compatible: *const u8,
            len: usize,
            ordinal: u32,
            out_device_id: *mut u32,
        ) -> i32;
        #[link_name = "kcore_mmio_claim"]
        pub fn kcore_mmio_claim(device_id: u32, out_handle: *mut u64) -> i32;
        #[link_name = "kcore_mmio_read_u32"]
        pub fn kcore_mmio_read_u32(handle: u64, offset: u32, out_value: *mut u32) -> i32;
        #[link_name = "kcore_mmio_write_u32"]
        pub fn kcore_mmio_write_u32(handle: u64, offset: u32, value: u32) -> i32;
        #[link_name = "kcore_mmio_release"]
        pub fn kcore_mmio_release(handle: u64) -> i32;
        #[link_name = "kcore_mmio_lease"]
        pub fn kcore_mmio_lease(handle: u64, out_ptr: *mut usize, out_len: *mut usize) -> i32;

        // -- Resource authority: DMA --
        #[link_name = "kcore_dma_alloc"]
        pub fn kcore_dma_alloc(
            mmio_handle: u64,
            size: usize,
            direction: i32,
            out_handle: *mut u64,
        ) -> i32;
        #[link_name = "kcore_dma_lease"]
        pub fn kcore_dma_lease(
            handle: u64,
            out_ptr: *mut usize,
            out_len: *mut usize,
            out_device_addr: *mut u64,
        ) -> i32;
        #[link_name = "kcore_dma_release"]
        pub fn kcore_dma_release(handle: u64) -> i32;

        // -- Resource authority: IRQ --
        #[link_name = "kcore_irq_claim"]
        pub fn kcore_irq_claim(mmio_handle: u64, out_handle: *mut u64) -> i32;
        #[link_name = "kcore_irq_register"]
        pub fn kcore_irq_register(handle: u64, handler: IrqHandler, ctx: *mut ()) -> i32;
        #[link_name = "kcore_irq_enable"]
        pub fn kcore_irq_enable(handle: u64) -> i32;
        #[link_name = "kcore_irq_register_polled"]
        pub fn kcore_irq_register_polled(handle: u64) -> i32;
        #[link_name = "kcore_irq_poll"]
        pub fn kcore_irq_poll(handle: u64, out_count: *mut u64) -> i32;
        #[link_name = "kcore_irq_ack"]
        pub fn kcore_irq_ack(handle: u64) -> i32;
        #[link_name = "kcore_irq_release"]
        pub fn kcore_irq_release(handle: u64) -> i32;
    }
}

// ---------------------------------------------------------------------------
// Interface binding：Core `kcore_interface_*` 之上的薄安全 wrapper
// ---------------------------------------------------------------------------

/// Component → Component Service binding 的薄 SDK 层。
///
/// 这一层把 ABI 编码（kind / fingerprint）变成类型、把 `0/-errno` 变成
/// `Result`、把裸指针收窄成 [`binding::RawBinding`] / [`binding::ServiceBinding`]。
/// 契约由实现 [`binding::Service`] 的类型表达（provider 与 consumer 共享），本模块
/// 随附 `SchedulerPolicy` / `DriverProber` 两个契约；**不做**字符串函数查找 / 反射 /
/// 动态类型——KernelNative phase 1 就是 typed `#[repr(C)]` function table + direct call。
pub mod binding {
    use crate::abi;

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
        pub fn bind() -> Result<Self, i32> {
            Ok(Self::from_raw(bind(S::NAME, S::KIND, S::ABI)?))
        }

        /// 用已有 binding id refresh：Core 重新验证 provider 后返回最新快照。
        pub fn refresh(&mut self) -> Result<(), i32> {
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

    /// 发布接口（**只在 `kcomp_init` 期间有效**）：Core 记录 pending，
    /// `kcomp_init` 返回 0 后原子提交。返回 `Ok(())` = 已记录 pending。
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
    ) -> Result<(), i32> {
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
        if status == 0 { Ok(()) } else { Err(status) }
    }

    /// 类型化 publish：provider 发布 `S` 的 `#[repr(C)]` function table。
    ///
    /// # Safety
    /// 同 [`publish`]：`api` 必须指向 `'static` 且布局 = `S::Api` 的 function table，
    /// `ctx` 必须是 provider 存活期内有效的 opaque state。
    pub unsafe fn publish_service<S: Service>(api: *const S::Api, ctx: *mut ()) -> Result<(), i32> {
        // SAFETY: 调用方保证 `api` 布局 = `S::Api`（见 Safety）。
        unsafe { publish(S::NAME, S::KIND, S::ABI, api as *const (), ctx) }
    }

    /// consumer 按名 bind：Core exact-compare ABI + 验证 provider 后返回当前快照。
    pub fn bind(name: &[u8], kind: InterfaceKind, abi: InterfaceAbi) -> Result<RawBinding, i32> {
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
            Err(status)
        }
    }

    /// consumer 用已有 binding id refresh：Core exact-compare ABI + 重新验证
    /// provider 后返回最新快照（provider replacement 后无需 ELF reload）。
    pub fn refresh(binding: u64, abi: InterfaceAbi) -> Result<RawBinding, i32> {
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
            Err(status)
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
        pub report_attempt:
            extern "C" fn(ctx: *mut (), attempt: u32, outcome: i32, detail: u32) -> i32,
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
}

// ---------------------------------------------------------------------------
// DMA 方向：ABI 编码的类型化镜像
// ---------------------------------------------------------------------------

/// DMA 传输方向。**这是 Component ABI 的一部分**：编码 `0/1/2`，与 Core
/// `handle/dma.rs::DmaDirection::as_i32` 及 `kcore_dma_alloc` 的 `direction`
/// 参数一致（见 `docs/driver-model.md` §6.2）。
///
/// Core 与 SDK 各自持有一份声明（组件不能依赖 `os/core`——那会把 Core 的 Rust
/// 类型与代码带进 `.kcomp`，违反"不建 shared runtime / 组件只经 `kcore_*` 交互"）；
/// 两侧各有锚定测试把值钉死在 0/1/2，防止漂移。设备库的枚举（如
/// `virtio_drivers::BufferDirection`）到本枚举的映射写在**驱动组件**里（纯类型
/// 匹配，不出现数字）。
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaDirection {
    /// 内存 → 设备。
    ToDevice = 0,
    /// 设备 → 内存。
    FromDevice = 1,
    /// 双向。
    Bidirectional = 2,
}

impl DmaDirection {
    /// ABI 编码（`#[repr(i32)]`，恒等于判别值）。
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

// ---------------------------------------------------------------------------
// 日志：固定缓冲 + kcore_log_line（无 alloc、无锁；行尾由 Core 追加）
// ---------------------------------------------------------------------------

/// 单行诊断上限（超长截断；panic 路径不需要无限长消息）。
const LOG_LINE_BYTES: usize = 256;

/// 把 `core::fmt` 写进固定栈缓冲。
struct LineBuffer<'a> {
    bytes: &'a mut [u8],
    length: usize,
}

impl core::fmt::Write for LineBuffer<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let source = value.as_bytes();
        let remaining = self.bytes.len() - self.length;
        let take = source.len().min(remaining);
        self.bytes[self.length..self.length + take].copy_from_slice(&source[..take]);
        self.length += take;
        Ok(())
    }
}

/// 输出一行：`core::fmt` 格式化到栈缓冲，再一次 `kcore_log_line`（Core 加
/// `[kcomp] ` 前缀与换行）。不分配、不持锁，可在 panic 路径使用。
pub fn log(args: core::fmt::Arguments<'_>) {
    let mut bytes = [0u8; LOG_LINE_BYTES];
    let mut writer = LineBuffer {
        bytes: &mut bytes,
        length: 0,
    };
    let _ = core::fmt::write(&mut writer, args);
    let length = writer.length;
    // SAFETY: (ptr, len) 指向本帧内已初始化的字节；Core 只读该区间。
    unsafe {
        abi::kcore_log_line(bytes.as_ptr(), length);
    }
}

/// 组件日志宏：`klog!("state={}", value)` → [`log`]。
#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => {
        $crate::log(::core::format_args!($($arg)*))
    };
}

/// 直写一字节（无缓冲；需要与日志行交错时用）。
pub fn console_write_byte(byte: u8) {
    // SAFETY: Core 保证该导出线程/中断安全地写 arch Console backend。
    unsafe {
        abi::kcore_console_write_byte(byte);
    }
}

// ---------------------------------------------------------------------------
// 组件入口约定
// ---------------------------------------------------------------------------

/// 定义加载入口 `kcomp_init`（Linux module_init 风格）。
///
/// 用法：`kcomp_sdk::kcomp_init!({ ...; 0 })`。块的值即返回码：`0` = 成功，
/// 非 0 = 失败位图（Core 据此标记 Failed）。与手写
/// `#[unsafe(no_mangle)] pub extern "C" fn kcomp_init() -> i32` 完全等价。
#[macro_export]
macro_rules! kcomp_init {
    ($($body:tt)*) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn kcomp_init() -> i32 {
            $($body)*
        }
    };
}

// 可选退出入口 `kcomp_exit`（Linux module_exit 风格）。
//
// 组件**可以**手写与 `kcomp_init!` 对称的 C ABI 退出入口：
//
//     #[unsafe(no_mangle)]
//     pub extern "C" fn kcomp_exit() -> i32 { 0 }
//
// 本阶段只完成生命周期**形状**：Core loader 会可选地解析该符号并存为 seam
// （`os/core/src/component/loader.rs::LoadedComponent::exit`、
// `component::registry::ComponentRecord::exit`），但**从不调用**它——未来的
// ComponentManager stop 路径才会真正驱动（`Ready → Stopping → Stopped`，见
// `component::ComponentState`）。不导出该符号是正常情况（`exit == None`）。
//
// 返回码约定（future，与 `kcomp_init` 对称）：`0` = 干净退出，非 0 = 失败
// 位图；当前未驱动，Core 侧无处理语义。

// ---------------------------------------------------------------------------
// panic adapter（组件私有；见 crate 文档）
// ---------------------------------------------------------------------------

/// 组件 panic handler：打印诊断后协作式逃逸回 Core。
///
/// 只在裸机目标生成——host 构建（clippy / workspace test）下 std 自带 panic
/// handler，重复定义会冲突。
#[cfg(target_os = "none")]
#[panic_handler]
fn component_panic(info: &core::panic::PanicInfo<'_>) -> ! {
    let mut bytes = [0u8; LOG_LINE_BYTES];
    let mut writer = LineBuffer {
        bytes: &mut bytes,
        length: 0,
    };
    // `kcore_log_line` 会加 `[kcomp] ` 前缀，这里只写消息体。
    let _ = core::fmt::write(&mut writer, format_args!("panic"));
    if let Some(location) = info.location() {
        let _ = core::fmt::write(
            &mut writer,
            format_args!(" at {}:{}", location.file(), location.line()),
        );
    }
    let _ = core::fmt::write(&mut writer, format_args!(": {}", info.message()));
    let length = writer.length;
    // SAFETY: 同 `log`：只读本帧缓冲；诊断必须活到 escape 之前。
    unsafe {
        abi::kcore_log_line(bytes.as_ptr(), length);
    }
    // 活动边界内永不返回；无边界时返回 -EPERM → 停在原地（安全失败）。
    unsafe {
        abi::kcore_panic_escape();
    }
    loop {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// alloc adapter（feature `alloc`）：GlobalAlloc → Core 共享堆
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "none", feature = "alloc"))]
mod global_alloc {
    use crate::abi;
    use core::alloc::{GlobalAlloc, Layout};

    /// 薄 adapter：不拥有内存，只把 Rust `GlobalAlloc` 契约转成 Core 共享堆 ABI。
    struct CoreHeap;

    unsafe impl GlobalAlloc for CoreHeap {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: layout 由 GlobalAlloc 契约保证合法（size>0、align 为 2 的幂）；
            // Core 侧再次校验，失败返回 null。
            unsafe { abi::kcore_heap_alloc(layout.size(), layout.align()) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: ptr 来自同一 layout 的一次成功 alloc（GlobalAlloc 契约）。
            unsafe {
                let _ = abi::kcore_heap_dealloc(ptr, layout.size(), layout.align());
            }
        }
    }

    #[global_allocator]
    static CORE_HEAP: CoreHeap = CoreHeap;
}

#[cfg(test)]
mod tests {
    //! host 锚定测试：钉住 ABI 编码（`docs/driver-model.md` §6.2）。
    //! Core 侧有对应测试 `component::export::tests::dma_direction_encoding_is_stable`。

    #[test]
    fn dma_direction_encoding_is_stable() {
        use super::DmaDirection::{Bidirectional, FromDevice, ToDevice};
        assert_eq!(ToDevice.as_i32(), 0);
        assert_eq!(FromDevice.as_i32(), 1);
        assert_eq!(Bidirectional.as_i32(), 2);
    }

    /// InterfaceKind ABI 编码锚定（与 Core `export.rs::kind_from_u32` 一致）。
    #[test]
    fn interface_kind_encoding_is_stable() {
        use super::binding::InterfaceKind::{Device, Policy, Service};
        assert_eq!(Device.as_u32(), 0);
        assert_eq!(Service.as_u32(), 1);
        assert_eq!(Policy.as_u32(), 2);
    }

    /// SchedulerPolicy ABI fingerprint 锚定：与 Core `sched::SCHEDULER_POLICY_ABI`
    /// 必须是同一数值（A/B 双侧手工锚定）。
    #[test]
    fn scheduler_policy_abi_is_anchored() {
        assert_eq!(
            super::binding::SCHEDULER_POLICY_ABI.raw(),
            0x5343_4845_4455_4C52
        );
    }

    /// driver.prober ABI fingerprint 锚定（ASCII "DRVPROBE"）：prober 与 driver
    /// 由完全相同的契约编译——数值漂移会让 bind 直接拒绝，这里把它钉死。
    #[test]
    fn driver_prober_abi_is_anchored() {
        assert_eq!(
            super::binding::DRIVER_PROBER_ABI.raw(),
            0x4452_5650_524F_4245
        );
    }

    /// 分配接口名字锚定：publish / bind 两侧必须逐字节一致。
    #[test]
    fn driver_prober_name_is_anchored() {
        assert_eq!(super::binding::DRIVER_PROBER_NAME, b"driver.prober");
    }

    /// `report_attempt` outcome 编码锚定（0 = Match，1 = NoMatch）。
    #[test]
    fn assign_outcome_encoding_is_stable() {
        assert_eq!(super::binding::ASSIGN_MATCH, 0);
        assert_eq!(super::binding::ASSIGN_NO_MATCH, 1);
    }
}
