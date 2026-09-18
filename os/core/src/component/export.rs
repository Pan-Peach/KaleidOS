//! 组件 → Core 稳定 API（EXPORT_SYMBOL 教学版，v1）。
//!
//! # 白名单原则（与 oracle 设计一致）
//! - 导出即契约：表内条目锁定（名字 + C ABI 签名），永不做破坏性修改；
//! - 未导出的内核函数组件"看不见"——内核内部随便重构，组件零影响；
//! - 未导出符号 → loader `UnresolvedSymbol`，整次加载失败（exact-name resolution）；
//! - 组件侧声明方式：`unsafe extern "C" { #[link_name = "kcore_..."] ... }`，
//!   loader 重定位时按未 mangled 字节名精确匹配。
//!
//! # ABI 分类（v1 稳定 + v2 增量 + v3 资源 authority 起步）
//!
//! | 类别 | 符号 | 说明 |
//! |---|---|---|
//! | Runtime / shared heap | `kcore_heap_alloc` `kcore_heap_dealloc` | KernelNative 组件与 Core 共享堆的分配/释放（契约 = Rust `GlobalAlloc`）。**不是**物理区域/帧分配、**不是**地址空间变更——这些 authority 敏感操作永不裸导出 |
//! | Logging / diagnostics | `kcore_console_write_byte` `kcore_log_line` | 输出通道（传输在 arch `Console` backend） |
//! | Machine query | `kcore_machine_boot_hart` `kcore_machine_cpu_count` `kcore_machine_has_hart` | 已提交机器真相的只读查询 |
//! | System query | `kcore_free_page_count` `kcore_task_count` `kcore_component_count` | 已提交 Core 真相的只读查询 |
//! | Component lifecycle（v2） | `kcore_component_create` `kcore_component_load` `kcore_interface_publish` `kcore_interface_available` `kcore_interface_bind` `kcore_interface_refresh` | 组件实例创建/接口发布的**语义入口**（非裸 registry mutation；requester/provider 由 Core 从 create 上下文解析，不信任组件自报身份）。`create` 取 `(image_name, KcompCreateArgs)`：同名 artifact 复用已登记的常驻 image，产生新实例（一份 image、N 个实例）；`load` 是默认配置（`config_abi = 0`）的便利入口。接口用 **exact ABI fingerprint**（`u64`，无版本兼容语义）：publish 在 `kcomp_instance_create` 期间只记录 pending（staged），create 返回 0 后 Core 原子提交；consumer bind/refresh 时 Core 重新验证 provider 并返回 opaque `api/ctx/generation` |
//! | Task control（v2） | `kcore_task_create` `kcore_task_start` `kcore_task_yield` `kcore_task_exit` `kcore_task_state` | 任务生命周期的**语义入口**（entry 必须落在 requester 实例镜像内；`arg` opaque 原样透传，任务归属来自 Core 执行边界；状态推进过 Core 状态机验证） |
//! | Panic containment（v2） | `kcore_panic_escape` | 组件 panic adapter 协作式交还控制权给 Core（活动边界内永不返回；无边界 → `-EPERM`），见 `component/containment.rs` |
//! | Scheduler（v2） | `kcore_sched_run` | 把 CPU 交给调度器（propose → validate → commit → switch 全在 Core） |
//! | Resource authority（v3 起步） | `kcore_device_nth` `kcore_mmio_claim` `kcore_mmio_read_u32` `kcore_mmio_write_u32` `kcore_mmio_release` `kcore_mmio_lease` `kcore_irq_claim` `kcore_irq_register` `kcore_irq_enable` `kcore_irq_register_polled` `kcore_irq_poll` `kcore_irq_ack` `kcore_irq_release` | 设备身份/认领链（identity → root → derived）：`device_nth` = 纯发现（列候选，不授权，`DeviceId` 是 identity 不是 handle）；`mmio_claim` = 用 `DeviceId` 认领**确切设备**（不是"第一台匹配"）→ Core authorize → grant；`irq_claim` = 从 caller 已持有的 `MmioHandle` 派生**同一台设备**的中断线（`irq=None` → `-ENODEV`）；`dma_alloc` 同样从 `MmioHandle` 推导设备身份。`mmio_release` 在仍有 live IRQ/DMA 子 authority 时拒绝（`-EBUSY`）；`irq_release` 真的关断投递（撤销 slot + 关断控制器线）。IRQ 投递两态：`register` = trap 上下文回调；`register_polled` + `poll`/`ack` = 轮询（Core 计数并掩蔽，驱动任务读完计数后 `ack` 由 Core 重新放行）。常规访问组件拿到的只是 raw handle，**不是地址/中断号**；`kcore_mmio_lease` 额外派生 Core 校验过一次的 `(ptr, len)` + provenance（受信 KernelNative 直接访问，撤销为协作式，见 `handle/lease.rs`）。全部 `0 / -Errno`、值走 out 参数 |
//! | DMA authority（v3 起步） | `kcore_dma_alloc` `kcore_dma_lease` `kcore_dma_release` | `alloc` = 用 caller **已持有的 `MmioHandle`** 推导设备身份（绝不接受自报设备号）→ Core 分配物理连续 backing → grant `DmaHandle`；`lease` = Core 校验后派生 backing `(ptr, len)` + **设备可见地址**（v1 identity：== 物理基址，无 IOMMU）+ provenance；`release` = backing lease 进 Core 私有 QUARANTINE（**不 free**，设备可能仍在 DMA）。见 `handle/dma.rs`。全部 `0 / -Errno`、值走 out 参数 |
//!
//! # ABI 错误约定（v3 起）
//!
//! ```text
//! 0          success
//! -negative  failure: -Errno
//! ```
//!
//! `Errno` 是稳定、Linux/POSIX 风格的数值命名空间（`os/core/src/errno.rs`）；
//! 各子系统的内部错误（`TaskError` / `HandleError` / `ComponentLoadError` /
//! `SchedError` / `InterfaceError` / `MmioError`）保持各自为政，只在导出边界
//! 翻译成 `Errno`。
//!
//! **返回值形状**（按"能否失败"分类）：
//! - 可失败、无值 → `i32 status`（`0` / `-Errno`）；
//! - 可失败、有值 → `i32 status + out 参数`（值不混进返回值）；
//! - 不会失败（纯 query）→ 直接返回值，`0` 是普通值不是哨兵。
//!
//! **宽度规则**：`usize` 只用于"语义就是指针宽"的量（地址 `entry`、
//! `(ptr, len)` 长度、分配器 `size/align`）；counts/ids → `u32`（v3 起，v1 的
//! `usize` 已迁移）；布尔/编码 → `i32`；不透明句柄 → `u64`，只经 `status + out`
//! 回传。
//!
//! 旧 v1/v2 的 `id >= 0 / -Errno` 值型签名保持兼容，迁移单独评估。
//!
//! # 身份解析与 Failed 门禁
//!
//! - **principal = 最内层当前活动的 Core-managed 执行边界**
//!   （`containment::active_escape`）：组件任务 → task owner；
//!   `kcomp_instance_create`（含**嵌套创建**）→ 被创建的实例；嵌套 create
//!   返回/panic 后恢复上一层边界。
//!   所有 authority / task / interface 入口统一走 `RequestContext::ambient()` /
//!   `ambient_init()`，不再各自偏好当前任务 owner。
//! - **Failed 实例门禁**：获取 authority / 创建 work 的入口
//!   （`kcore_mmio_claim`、`kcore_irq_claim`、`kcore_dma_alloc`、`kcore_task_create`、
//!   `kcore_interface_publish`）在 caller/provider 已 `Failed` 时返回 `-EPERM`；
//!   `release` / `revoke` 及已持有 handle 的操作**不受此门禁限制**，teardown 仍可用。
//!
//! # 明确不导出（未经 Core validation 的裸 authority mutation）
//!
//! 组件可以 **request** 资源（v3+ 的 `kcore_device_nth` + `kcore_mmio_claim` = discover + request → Core
//! authorize → Core grant），但任何 Core truth 的 mutation 都必须由 Core
//! 验证后提交并留 trace；裸 mutation 入口一律不导出：
//!
//! - 物理内存：`memory::alloc_region` / `free_region` / `vm_page_alloc`——物理帧是
//!   Core 内部机制（canonical），组件要内存走 `kcore_heap_alloc`（共享堆）。
//! - 地址空间：`KernelAddressSpace::map/unmap/activate`——mutation 必须过 Core
//!   验证与 commit，且需要 `AddressSpaceHandle`（未来类型化授权 API）。
//! - 裸任务表：`TaskTable::create` / `set_task_state` / context switch——绕过 Core
//!   truth 的 mutation 一律不导出；v2 的 `kcore_task_*` 是**带验证的语义入口**
//!   （requester 校验 + entry 镜像校验 + 状态机），不是 `TaskTable` 的透传。
//! - 注册表：`registry::declare/start/unload`——组件生命周期由 Core 掌控，
//!   `kcore_component_create`（store → image 复用 → declare → resolve →
//!   start → kcomp_instance_create）与默认配置便利入口 `kcore_component_load`
//!   是完整语义请求（registry 无 unload：Stopped/Failed 记录留作 tombstone）。
//! - Trace 事件：组件未来只能提交"组件自定义事件"，`TaskSwitch/Grant/Revoke/
//!   CoreRejected` 等 Core authoritative event 由 Core 自己产生（TODO：trace
//!   环形缓冲落地后加 `kcore_trace_component_event`，sequence 由 Core 分配）。
//!
//! # 共享堆 ABI 的所有权/生命周期语义
//!
//! `kcore_heap_alloc/dealloc` 是 KernelNative 组件共享 Core 堆的入口（AGENTS.md：
//! Core 与组件共享一个 Core heap，无 per-component 记账）。契约与 Rust
//! `GlobalAlloc` 完全一致：dealloc 的 `(ptr, size, align)` 必须与一次成功的 alloc
//! 严格匹配，违反 = UB（与 C `malloc/free` 错配同类）。组件失败后的泄漏在 phase 1
//! 可接受（不承诺共享堆字节回收，见 roadmap §8）；完整回收留给未来 ExecutionDomain。

use crate::component::containment::KcompCreateArgs;
use crate::component::interface::{InterfaceAbi, InterfaceKind, get_interfaces};
use crate::component::registry;
use crate::errno::{Errno, status};
use crate::handle::{RequestContext, dma, irq, mmio};
use crate::machine;
use crate::memory;
use crate::sched;
use crate::task::{self, TaskId, TaskState};
use arch::{Console, ConsoleImpl, InterruptController, InterruptImpl};
use core::alloc::GlobalAlloc;

/// 单个导出条目：公开字节名 + 内核侧函数地址。
/// 地址以裸函数指针存静态——rustc 生成普通数据重定位，最终链接器填入真实地址，
/// 无需 build script / 运行时注册。
struct Export {
    name: &'static [u8],
    address: ExportAddress,
}

/// 包装裸函数指针：`Sync` 安全（条目不可变，指向已链接的可执行文本）。
#[repr(transparent)]
struct ExportAddress(*const ());

unsafe impl Sync for ExportAddress {}

// ---------------------------------------------------------------------------
// Category 1：Runtime / shared heap（KernelNative 组件共享堆）
// ---------------------------------------------------------------------------

/// 共享堆分配。契约 = Rust `GlobalAlloc::alloc`：`align` 必须为 2 的幂，
/// `size > 0`；失败返回 null。所有权归调用方组件；Core 不做 per-component 记账。
///
/// # Safety
/// 返回指针的释放必须通过 `kcore_heap_dealloc`（携带相同 size/align）。
extern "C" fn kcore_heap_alloc(size: usize, align: usize) -> *mut u8 {
    // C ABI 语义：size==0 或非法 align 一律失败返回 null。
    // （Rust `Layout` 允许空 layout，但 C 风格调用方可能传 0——显式拒绝。）
    if size == 0 {
        return core::ptr::null_mut();
    }
    let Ok(layout) = core::alloc::Layout::from_size_align(size, align) else {
        return core::ptr::null_mut();
    };
    // SAFETY: layout 已由 from_size_align 验证；KernelAllocator 是共享堆的
    // GlobalAlloc 实现（host test 下经 test_support::ensure_init 就绪）。
    unsafe { memory::KernelAllocator.alloc(layout) }
}

/// 共享堆释放。契约 = Rust `GlobalAlloc::dealloc`（见模块文档的语义说明）。
/// 返回 0 / `-Errno`（`EFAULT` 空指针 / `EINVAL` 非法 layout）。
///
/// # Safety
/// `ptr` 必须来自一次成功的 `kcore_heap_alloc`，且 `(size, align)` 必须与那次
/// 调用完全一致。违反 = UB。
extern "C" fn kcore_heap_dealloc(ptr: *mut u8, size: usize, align: usize) -> i32 {
    if ptr.is_null() {
        return Errno::EFAULT.code();
    }
    let Ok(layout) = core::alloc::Layout::from_size_align(size, align) else {
        return Errno::EINVAL.code();
    };
    // SAFETY: 由调用方保证 ptr/layout 匹配一次成功 alloc（C ABI 契约）。
    unsafe {
        memory::KernelAllocator.dealloc(ptr, layout);
    }
    0
}

// ---------------------------------------------------------------------------
// Category 2：Logging / diagnostics
// ---------------------------------------------------------------------------

extern "C" fn kcore_console_write_byte(byte: u8) {
    ConsoleImpl::write_byte(byte);
}

/// 输出一行（`[kcomp] ` 前缀）。返回 0 / `-Errno`（`EFAULT` 空指针 /
/// `EOVERFLOW` 长度超 `isize::MAX`）。
extern "C" fn kcore_log_line(ptr: *const u8, len: usize) -> i32 {
    if ptr.is_null() && len != 0 {
        return Errno::EFAULT.code();
    }
    if len > isize::MAX as usize {
        return Errno::EOVERFLOW.code();
    }
    const PREFIX: &[u8] = b"[kcomp] ";
    for &b in PREFIX {
        ConsoleImpl::write_byte(b);
    }
    if len > 0 {
        let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
        for &b in bytes {
            ConsoleImpl::write_byte(b);
        }
    }
    ConsoleImpl::write_byte(b'\n');
    0
}

// ---------------------------------------------------------------------------
// Category 0：Trace（只读观察面）
// ---------------------------------------------------------------------------

/// 读一条 trace 记录（组件用它断言事件序列 / 调试；**只读**，无 god-mode）。
///
/// 语义：把 `seq >= since` 的**第一条**记录写入调用者提供的 `out`，并通过
/// `out_next` 返回下一次应传的 `since`（= 本次记录 `seq` + 1）。没有更多记录时
/// 返回 `ENOENT` —— **不返回 0**，否则无法区分"读到了一条"与"读完了"。
///
/// 实现为 O(1)：直接在 ring 元数据上定位并拷贝一条（锁内无遍历、无回调、无分配），
/// 命中后立即返回。`since` 若已被逐出保留区，返回的是当前最旧存活记录——
/// reader 的缺口 = `record.seq - since`。
///
/// 不分配、不回调、不暴露 Core 内部指针：`out` / `out_next` 都是调用者自己的内存。
/// 记录形态是 `#[repr(C)]` 的稳定编码（见 `trace::abi`），不是 Rust enum layout。
extern "C" fn kcore_trace_read(
    since: u64,
    out: *mut crate::trace::TraceRecordAbi,
    out_next: *mut u64,
) -> i32 {
    if out.is_null() || out_next.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(record) = crate::trace::ring::read_one(since) else {
        return Errno::ENOENT.code();
    };
    let abi = crate::trace::TraceRecordAbi::from(&record);
    // SAFETY: 两个指针在上面已校验非空；它们是调用者提供的可写内存。
    unsafe {
        out.write(abi);
        out_next.write(record.seq.saturating_add(1));
    }
    0
}

/// 读 Trace 子系统状态（只读观察面）：容量 / 最旧 seq / 下一 seq / 逐出总数 /
/// 已使能事件掩码。值走调用者提供的 `out`。
///
/// 成功 = 0；失败 = `-Errno`（`EFAULT` 空指针）。
///
/// **损失语义**：`overwritten_total` 是"因 ring 满被逐出保留区"的记录总数，
/// 不是"某个 reader 漏掉的条数"——reader 的真实缺口是
/// `record.seq - since`（见 [`kcore_trace_read`]）。被事件使能掩码过滤的事件
/// 不记录、也不消耗 `seq`，不算丢失。
extern "C" fn kcore_trace_stats(out: *mut crate::trace::TraceStatsAbi) -> i32 {
    if out.is_null() {
        return Errno::EFAULT.code();
    }
    let stats = crate::trace::stats();
    let abi = crate::trace::TraceStatsAbi::from(&stats);
    // SAFETY: out 在上面已校验非空；它是调用者提供的可写内存。
    unsafe { out.write(abi) };
    0
}

// ---------------------------------------------------------------------------
// Category 3：Machine query（已提交机器真相的只读查询；counts/ids → u32）
// ---------------------------------------------------------------------------

extern "C" fn kcore_machine_boot_hart() -> u32 {
    machine::committed().map_or(0, |m| m.boot_hart as u32)
}

/// 单调时钟（`rdtime` 的原始 tick）——组件侧计时用，无 authority 语义。
///
/// 频率见 [`kcore_timebase_hz`]。注意真机上 timebase 常是 10 MHz（1 tick =
/// 100 ns），测很短的操作要么**累积多次再除**，要么等 cycle 源（`rdcycle`）。
extern "C" fn kcore_now() -> u64 {
    <arch::TimerImpl as arch::Timer>::now()
}

/// 时钟频率（Hz）：把 [`kcore_now`] 的 tick 换算成时间需要它。
extern "C" fn kcore_timebase_hz() -> u64 {
    machine::committed().map_or(0, |m| m.timebase_frequency)
}

extern "C" fn kcore_machine_cpu_count() -> u32 {
    machine::committed().map_or(0, |m| m.cpu_count as u32)
}

extern "C" fn kcore_machine_has_hart(hart_id: u32) -> i32 {
    let Some(machine) = machine::committed() else {
        return 0;
    };
    machine.cpu_info[..machine.cpu_count.min(machine.cpu_info.len())]
        .iter()
        .any(|cpu| cpu.hart_id.raw() == hart_id as usize) as i32
}

// ---------------------------------------------------------------------------
// Category 4：System query（已提交 Core 真相的只读查询；counts → u32）
// ---------------------------------------------------------------------------

extern "C" fn kcore_free_page_count() -> u32 {
    memory::free_block_counts()
        .iter()
        .enumerate()
        .skip(memory::HEAP_MIN_ORDER)
        .map(|(order, &blocks)| blocks * (1usize << (order - memory::HEAP_MIN_ORDER)))
        .sum::<usize>() as u32
}

extern "C" fn kcore_task_count() -> u32 {
    task::get_task_table().lock().len() as u32
}

extern "C" fn kcore_component_count() -> u32 {
    registry::get_registry().lock().len() as u32
}

// ---------------------------------------------------------------------------
// Category 5：Component lifecycle（v2；语义入口，非裸 registry mutation）
// ---------------------------------------------------------------------------

/// C ABI `(ptr, len)` → 短切片。长度上限防御野指针/超长输入（组件名受
/// `MAX_NAME_LEN` 约束）。返回的切片只在调用期间有效。
fn checked_name(ptr: *const u8, len: usize) -> Option<&'static [u8]> {
    if ptr.is_null() || len == 0 || len > 256 {
        return None;
    }
    // SAFETY: 调用方保证 (ptr, len) 指向调用期间有效的内存。
    Some(unsafe { core::slice::from_raw_parts(ptr, len) })
}

/// `InterfaceKind` 的 ABI 编码（与枚举声明序一致：0=Device 1=Service
/// 2=Policy；改动枚举声明序 = ABI 破坏，必须同步 bump 文档）。
fn kind_from_u32(kind: u32) -> Option<InterfaceKind> {
    match kind {
        0 => Some(InterfaceKind::Device),
        1 => Some(InterfaceKind::Service),
        2 => Some(InterfaceKind::Policy),
        _ => None,
    }
}

/// 请求 Core 用**默认配置**创建组件实例（store → image 复用/加载 → registry →
/// `kcomp_instance_create` 全链，与 monitor `load` 同源）。
///
/// 这是 `kcore_component_create` 的便利入口（`config_abi = 0`，无 config 负载）。
/// 返回 ComponentId raw（≥ 0）/ `-Errno`
/// （`EINVAL` 名字非法；其余见 `Errno::from(ComponentLoadError)`）。
extern "C" fn kcore_component_load(name_ptr: *const u8, name_len: usize) -> i32 {
    let Some(name) = checked_name(name_ptr, name_len) else {
        return Errno::EINVAL.code();
    };
    match crate::component::load::load_and_start(name) {
        Ok(id) => id.raw() as i32,
        Err(error) => Errno::from(error).code(),
    }
}

/// 用指定 config 负载创建一个新实例（`docs/component-lifecycle.md` §4）。
///
/// 同名 artifact 复用已登记的常驻 image（新实例、新 id、新 state）；否则
/// store → loader → image 登记。`args` 是组件自定义的 C 布局小结构，Core 视为
/// **不透明字节**（只在调用期间借用，不持久化、不解释）。
///
/// 成功 = 0，实例 id（`u32`）写入 `*out_instance`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` `args` / `out_instance` 为空 / `EINVAL` 名字非法 /
/// 其余见 `Errno::from(ComponentLoadError)`）。
extern "C" fn kcore_component_create(
    image_name: *const u8,
    image_name_len: usize,
    args: *const KcompCreateArgs,
    out_instance: *mut u32,
) -> i32 {
    if args.is_null() || out_instance.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(name) = checked_name(image_name, image_name_len) else {
        return Errno::EINVAL.code();
    };
    // SAFETY: 调用方保证 args 指向调用期间有效的 KcompCreateArgs（C ABI 契约）；
    // Core 只在本次调用内借用它。
    let args = unsafe { &*args };
    match crate::component::load::create_component(name, args) {
        Ok(id) => {
            // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
            unsafe { core::ptr::write_unaligned(out_instance, id.raw()) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 发布接口（**staged**：`kcomp_instance_create` 期间只记录 pending，不修改
/// active binding）。provider = 当前正在创建的实例（Core 记录，**不信任组件自报身份**）。
///
/// `abi` 是 exact ABI fingerprint（`u64`，无版本兼容语义）：provider 与 consumer
/// 必须由完全相同的 Service ABI contract 编译。`api` 指向 provider 的 `#[repr(C)]`
/// function table，`ctx` 是 provider opaque state；Core 只存指针、永不解引用。
///
/// create 返回 0 后 Core 原子提交该实例的 pending interfaces；ABI 冲突的
/// replacement 在提交时被拒绝。因此本函数返回 `0` 只表示"已记录 pending"。
/// provider 由最内层活动 create 边界解析（嵌套创建 = 被创建的实例），不信任组件
/// 自报身份；`Failed` provider → `-EPERM`。
/// 返回 0 / `-Errno`（`EINVAL` 名字/kind 非法；`EPERM` 不在 create 上下文或
/// provider 已 `Failed`；其余见 `Errno::from(InterfaceError)`）。
extern "C" fn kcore_interface_publish(
    name_ptr: *const u8,
    name_len: usize,
    kind: u32,
    abi: u64,
    api: *const (),
    ctx: *mut (),
) -> i32 {
    let Some(name) = checked_name(name_ptr, name_len) else {
        return Errno::EINVAL.code();
    };
    let Some(kind) = kind_from_u32(kind) else {
        return Errno::EINVAL.code();
    };
    let Some(provider) = RequestContext::ambient_init().map(|ctx| ctx.component) else {
        return Errno::EPERM.code();
    };
    if let Some(denied) = deny_if_failed(provider) {
        return denied;
    }
    let reg = registry::get_registry().lock();
    let mut ifs = get_interfaces().lock();
    match ifs.stage_publish(
        &reg,
        provider,
        name,
        kind,
        InterfaceAbi::from_raw(abi),
        api,
        ctx,
    ) {
        Ok(()) => 0,
        Err(error) => Errno::from(error).code(),
    }
}

/// 只读查询：`(name, kind, abi)` 是否已绑定且 provider 存活（Ready）。
/// 1 = 可用（可 bind），0 = 不可用。
extern "C" fn kcore_interface_available(
    name_ptr: *const u8,
    name_len: usize,
    kind: u32,
    abi: u64,
) -> i32 {
    let (Some(name), Some(kind)) = (checked_name(name_ptr, name_len), kind_from_u32(kind)) else {
        return 0;
    };
    let reg = registry::get_registry().lock();
    let ifs = get_interfaces().lock();
    ifs.bind(&reg, name, kind, InterfaceAbi::from_raw(abi))
        .is_ok() as i32
}

/// consumer 按名 bind：Core 查找接口 → exact-compare ABI fingerprint → 验证当前
/// provider 存在且 Ready → 返回稳定 `BindingId` + 当前 `api/ctx/generation`。
///
/// 成功 = 0，`*out_binding`（`u64`）、`*out_api` / `*out_ctx`（指针宽 `usize`）、
/// `*out_generation`（`u64`）写入（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` 任一 out 为空 / `EINVAL` 名字/kind 非法 /
/// `ENOENT` 接口未知·Unbound / ABI mismatch 等见 `Errno::from(InterfaceError)`）。
#[allow(clippy::too_many_arguments)]
extern "C" fn kcore_interface_bind(
    name_ptr: *const u8,
    name_len: usize,
    kind: u32,
    abi: u64,
    out_binding: *mut u64,
    out_api: *mut usize,
    out_ctx: *mut usize,
    out_generation: *mut u64,
) -> i32 {
    if out_binding.is_null() || out_api.is_null() || out_ctx.is_null() || out_generation.is_null() {
        return Errno::EFAULT.code();
    }
    let (Some(name), Some(kind)) = (checked_name(name_ptr, name_len), kind_from_u32(kind)) else {
        return Errno::EINVAL.code();
    };
    let reg = registry::get_registry().lock();
    let ifs = get_interfaces().lock();
    match ifs.bind(&reg, name, kind, InterfaceAbi::from_raw(abi)) {
        Ok(view) => {
            // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
            unsafe {
                core::ptr::write_unaligned(out_binding, view.id.raw() as u64);
                core::ptr::write_unaligned(out_api, view.api as usize);
                core::ptr::write_unaligned(out_ctx, view.ctx as usize);
                core::ptr::write_unaligned(out_generation, view.generation);
            }
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// consumer 用已有 `BindingId` refresh：exact-compare 期望 ABI → 重新验证 provider →
/// 返回最新 `api/ctx/generation`（provider 替换后无需 ELF reload）。
///
/// 成功 = 0，三个 out 写入（调用方保证可写，任意对齐）；失败 = `-Errno`
/// （`EFAULT` 任一 out 为空 / ABI mismatch / Unbound / BindingNotFound）。
extern "C" fn kcore_interface_refresh(
    binding: u64,
    abi: u64,
    out_api: *mut usize,
    out_ctx: *mut usize,
    out_generation: *mut u64,
) -> i32 {
    if out_api.is_null() || out_ctx.is_null() || out_generation.is_null() {
        return Errno::EFAULT.code();
    }
    // `binding` 是 u64 ABI 宽度；BindingId 是 u32。超范围直接拒绝（不得截断后
    // 命中一个无关的槽）。
    let Ok(binding) = u32::try_from(binding) else {
        return Errno::ENOENT.code();
    };
    let reg = registry::get_registry().lock();
    let ifs = get_interfaces().lock();
    match ifs.refresh(
        &reg,
        crate::component::interface::BindingId::from_raw(binding),
        InterfaceAbi::from_raw(abi),
    ) {
        Ok(view) => {
            // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
            unsafe {
                core::ptr::write_unaligned(out_api, view.api as usize);
                core::ptr::write_unaligned(out_ctx, view.ctx as usize);
                core::ptr::write_unaligned(out_generation, view.generation);
            }
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

// ---------------------------------------------------------------------------
// Category 6：Task control（v2；语义入口，authority 校验在 Core）
// ---------------------------------------------------------------------------

/// 解析 Core API caller：身份统一走 [`RequestContext::ambient`]——最内层活动执行
/// 边界优先（组件任务 → task owner；`kcomp_instance_create`，含嵌套创建 → 被创建的实例）。
fn current_task_requester() -> Option<crate::component::ComponentId> {
    RequestContext::ambient().map(|ctx| ctx.component)
}

/// Core 真相门禁：拒绝来自 `Failed` 实例的「获取 authority / 创建 work」请求。
///
/// 失败实例逻辑死亡，可 teardown（`release` / `revoke`、释放已持有 handle），
/// 但不得获取新 authority 或创建新 work。返回 `-EPERM`（Core 策略拒绝，与
/// 「无法解析 caller」同一 errno）。**只有** acquiring/creating 入口调用本函数。
fn deny_if_failed(component: crate::component::ComponentId) -> Option<i32> {
    crate::component::is_failed(component).then_some(Errno::EPERM.code())
}

/// 创建任务。requester = 当前 caller；`entry` 必须落在该实例的
/// 装载镜像内（越界指针一律拒绝）；`arg` 是 opaque 参数，Core 原样透传给任务
/// 入口（`typedef void (*)(void *)`），**任务归属仍来自 Core 的执行边界**，
/// 与 `arg` 内容无关。
///
/// 成功 = 0，TaskId（`u32`）写入 `*out_task`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller；
/// 其余见 `Errno::from(TaskError)`）。
extern "C" fn kcore_task_create(entry: usize, arg: *mut (), out_task: *mut u32) -> i32 {
    if out_task.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(requester) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    if let Some(denied) = deny_if_failed(requester) {
        return denied;
    }
    match task::create_task(requester, entry, arg) {
        Ok(id) => {
            // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
            unsafe { core::ptr::write_unaligned(out_task, id.raw()) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 启动任务：Core 验证当前 caller 是任务 owner 后才推进 Created → Runnable。
/// 返回 0 / `-Errno`。
extern "C" fn kcore_task_start(id: u32) -> i32 {
    let Some(requester) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    status(task::start_task(requester, TaskId::from_raw(id)))
}

/// 让出 CPU：Running → Runnable + 调度切换。任务再次被选中时返回 0。
/// 返回 0 / `-Errno`。
extern "C" fn kcore_task_yield() -> i32 {
    status(sched::yield_current())
}

/// 退出：Running → Exited + 调度切换。**控制权永不回到本任务**——若还有
/// Runnable 任务则它们接管；全部退出后回到调度器锚点（调 `kcore_sched_run`
/// 的上下文）。返回 0 / `-Errno`。
extern "C" fn kcore_task_exit() -> i32 {
    status(sched::exit_current())
}

/// 只读查询任务状态（Core 真相的编码视图）：
/// 0=Created 1=Runnable 2=Running 3=Blocked 4=Exited；`-ESRCH` = 不存在。
extern "C" fn kcore_task_state(id: u32) -> i32 {
    let table = task::get_task_table().lock();
    match table.get(TaskId::from_raw(id)) {
        None => Errno::ESRCH.code(),
        Some(record) => match record.state() {
            TaskState::Created => 0,
            TaskState::Runnable => 1,
            TaskState::Running(_) => 2,
            TaskState::Blocked => 3,
            TaskState::Exited => 4,
        },
    }
}

// ---------------------------------------------------------------------------
// Category 6（续）：Panic containment（v2；协作式 escape）
// ---------------------------------------------------------------------------

/// 组件 panic 协作式逃逸。组件 SDK 的 panic adapter 打印诊断后调用此入口，
/// 把控制权交还 Core（在活动 containment 边界内会标记该 instance Failed 并切回
/// Core 上下文，见 `component/containment.rs`）。
///
/// 语义：活动边界存在时触发 escape，**本函数永不返回**（控制权切回 Core，与 boot
/// `#[panic_handler]` 直接调 `containment::panic_escape()` 等价，只是组件只能经
/// 白名单符号调用）；无活动边界时返回 `-EPERM`（当前执行不在任何组件边界内，
/// 没有可逃逸的目标）。
extern "C" fn kcore_panic_escape() -> i32 {
    // `panic_escape()` 只在无活动边界时返回（`false`）；有活动边界时它会切换回
    // Core 而不会到达这里。因此任何返回都意味着"未发生逃逸"——报告 `-EPERM`
    // 而不假装成功（`true` 在健康上下文后端下不可达）。
    let _escaped = crate::component::containment::panic_escape();
    Errno::EPERM.code()
}

// ---------------------------------------------------------------------------
// Category 7：Scheduler（v2）
// ---------------------------------------------------------------------------

/// 把 CPU 交给调度器：跑完所有 Runnable 任务后返回（锚点上下文）。
/// 无 Runnable 任务时为 no-op。返回 0 / `-Errno`（见 `Errno::from(SchedError)`）。
extern "C" fn kcore_sched_run() -> i32 {
    status(sched::run())
}

// ---------------------------------------------------------------------------
// Category 8：Resource authority（v3 起步；request → authorize → grant → access）
// ---------------------------------------------------------------------------

/// 纯设备发现：按 compatible 取第 `ordinal` 个匹配描述符。
///
/// **不分配、不预留、不触碰任何设备寄存器、不读取 claim 状态**；枚举包含已认领
/// 设备，顺序只取决于已提交的 `MachineInfo`（跨 claim/release 稳定）。一条描述符
/// 匹配任意 compatible 串只计一次。
///
/// 成功 = 0，`DeviceId`（纯身份，**不是 handle、不可撤销、无权限**；`u32`）写入
/// `*out_device_id`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EINVAL` compatible 非法 /
/// `ENODEV` 机器信息尚未提交 / `ENOENT` `ordinal` 超出匹配数——**唯一终止信号**）。
extern "C" fn kcore_device_nth(
    compatible_ptr: *const u8,
    compatible_len: usize,
    ordinal: u32,
    out_device_id: *mut u32,
) -> i32 {
    if out_device_id.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(compatible) = checked_name(compatible_ptr, compatible_len) else {
        return Errno::EINVAL.code();
    };
    match machine::nth_compatible(compatible, ordinal) {
        Ok(device) => {
            // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
            unsafe { core::ptr::write_unaligned(out_device_id, device.raw()) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 认领**一台确切设备**的 MMIO root authority（v2）。
///
/// 语义：`device_id`（发现阶段得到）→ Core 解析到设备记录 → authorize（phase 1 恒
/// allow）→ 独占检查（该 `device_index` 已被认领或处于失败 quarantine 则拒绝）→
/// grant `MmioHandle`。独占锚在**设备**上：同一设备的 IRQ / DMA 只能从这个 root 派生。
///
/// 链接名沿用原名，签名直接替换（收 `device_id` 而非 compatible）；本阶段不提供
/// ABI 兼容，所有内置组件一同重编，不保证陈旧 `.kcomp` 可加载。
///
/// 成功 = 0，raw handle（`to_raw` 编码：高 32 位 slot、低 32 位 generation；
/// **不是地址**）写入 `*out_handle`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller、caller 已
/// `Failed`、或 Core 策略拒绝 / `ENODEV` 设备不存在 / `ENOTSUP` 设备是 PIO /
/// `EBUSY` 设备已被认领或已 quarantine）。
extern "C" fn kcore_mmio_claim(device_id: u32, out_handle: *mut u64) -> i32 {
    if out_handle.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    if let Some(denied) = deny_if_failed(ctx.component) {
        return denied;
    }
    match mmio::claim_device(&ctx, machine::DeviceId::from_raw(device_id)) {
        Ok(handle) => {
            // SAFETY: `out_handle` 的可写性由调用方保证（C ABI 契约）；unaligned
            // 写避免调用方指针未对齐 = UB。
            unsafe { core::ptr::write_unaligned(out_handle, handle.to_raw()) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 单次 32-bit MMIO 读（C6 起步）。每次调用 Core 都重新验证 handle，
/// 过 bounds/对齐检查后由 Core 访问硬件；组件永远拿不到地址。
///
/// 成功 = 0，值写入 `*out_value`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller /
/// `EBADF` slot 不存在 / `ESTALE` 过期 / `EACCES` 非 owner /
/// `EKEYREVOKED` 已 revoke / `EALREADY` 已释放 / `EINVAL` 越界或未对齐）。
extern "C" fn kcore_mmio_read_u32(handle: u64, offset: u32, out_value: *mut u32) -> i32 {
    if out_value.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    match mmio::read_u32(&ctx, mmio::MmioHandle::from_raw(handle), offset) {
        Ok(value) => {
            // SAFETY: 同 `kcore_mmio_claim`（调用方保证可写；unaligned 写防未对齐 UB）。
            unsafe { core::ptr::write_unaligned(out_value, value) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 单次 32-bit MMIO 写。每次调用 Core 都重新验证 handle，组件永远拿不到地址。
/// 返回 0 / `-Errno`。
extern "C" fn kcore_mmio_write_u32(handle: u64, offset: u32, value: u32) -> i32 {
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    status(mmio::write_u32(
        &ctx,
        mmio::MmioHandle::from_raw(handle),
        offset,
        value,
    ))
}

/// 主动释放 MMIO authority。返回 0 / `-Errno`。
extern "C" fn kcore_mmio_release(handle: u64) -> i32 {
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    status(mmio::release(&ctx, mmio::MmioHandle::from_raw(handle)))
}

/// 派生 `MmioView`：Core 校验 handle 后一次性给出 `(ptr, len)`（KernelNative
/// 直接 MMIO 快路径，C6 起步）。受信 KernelNative 驱动据此直接 volatile 访问，
/// 稳态不再 per-access 进 Core；provenance 绑定 `source` handle。**撤销是协作
/// 式的**：Core 撤销 authority 后不会追回已经派生出去的裸指针。
///
/// 成功 = 0，指针写入 `*out_ptr`、长度写入 `*out_len`（调用方保证可写，任意
/// 对齐）；失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller /
/// handle 类错误同 `kcore_mmio_read_u32`）。
extern "C" fn kcore_mmio_lease(handle: u64, out_ptr: *mut usize, out_len: *mut usize) -> i32 {
    if out_ptr.is_null() || out_len.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    match mmio::derive_lease(&ctx, mmio::MmioHandle::from_raw(handle)) {
        Ok(lease) => {
            // SAFETY: 两个 out 指针的可写性由调用方保证（C ABI 契约）；unaligned
            // 写避免调用方指针未对齐 = UB。
            unsafe {
                core::ptr::write_unaligned(out_ptr, lease.as_ptr() as usize);
                core::ptr::write_unaligned(out_len, lease.len());
            }
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

// ---------------------------------------------------------------------------
// Category 8（续）：DMA authority（v3 起步；alloc → lease → release）
// ---------------------------------------------------------------------------

/// `DmaDirection` 的 ABI 编码（与枚举声明序一致：0=ToDevice 1=FromDevice
/// 2=Bidirectional；改动枚举声明序 = ABI 破坏，必须同步 bump 文档）。
fn dma_direction_from_i32(direction: i32) -> Option<dma::DmaDirection> {
    match direction {
        0 => Some(dma::DmaDirection::ToDevice),
        1 => Some(dma::DmaDirection::FromDevice),
        2 => Some(dma::DmaDirection::Bidirectional),
        _ => None,
    }
}

/// 分配一段 Core 拥有的物理连续 DMA 区域（driver-model step 3）。
///
/// 语义：`mmio_handle` 必须是 caller **已经持有**的设备 MMIO authority；Core
/// 从它推导 `device_index`（**不接受组件自报设备号**）→ `alloc_region` 分配
/// backing → grant `DmaHandle`。组件只拿到 raw handle，拿不到地址。
/// 成功 = 0，raw handle 写入 `*out_handle`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EINVAL` direction 非法或尺寸非法 /
/// `EPERM` 无法解析 caller 或 caller 已 `Failed` / `ENODEV` 无匹配设备 /
/// `EBUSY` 设备已被认领 / `ENOMEM` 物理内存耗尽 / handle 类错误同
/// `kcore_mmio_read_u32`）。
extern "C" fn kcore_dma_alloc(
    mmio_handle: u64,
    size: usize,
    direction: i32,
    out_handle: *mut u64,
) -> i32 {
    if out_handle.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(direction) = dma_direction_from_i32(direction) else {
        return Errno::EINVAL.code();
    };
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    if let Some(denied) = deny_if_failed(ctx.component) {
        return denied;
    }
    match dma::alloc(
        &ctx,
        mmio::MmioHandle::from_raw(mmio_handle),
        size,
        direction,
    ) {
        Ok(handle) => {
            // SAFETY: `out_handle` 的可写性由调用方保证（C ABI 契约）；unaligned
            // 写避免调用方指针未对齐 = UB。
            unsafe { core::ptr::write_unaligned(out_handle, handle.to_raw()) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 派生 `DmaView`：Core 校验 handle 后一次性给出 backing `(ptr, len)` +
/// **设备可见地址**（v1 identity：等于物理基址，无 IOMMU）+ provenance
/// （`source` handle）。受信 KernelNative 驱动据此直接 DMA；撤销是协作式，
/// 且 backing 只进 quarantine（不 free）。
///
/// 成功 = 0，三个 out（调用方保证可写、任意对齐）分别写入 ptr / len /
/// device_addr；失败 = `-Errno`（`EFAULT` 任一 out 为空 / `EPERM` 无法解析
/// caller / handle 类错误同 `kcore_mmio_read_u32`）。
extern "C" fn kcore_dma_lease(
    handle: u64,
    out_ptr: *mut usize,
    out_len: *mut usize,
    out_device_addr: *mut u64,
) -> i32 {
    if out_ptr.is_null() || out_len.is_null() || out_device_addr.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    match dma::derive_lease(&ctx, dma::DmaHandle::from_raw(handle)) {
        Ok(lease) => {
            // SAFETY: 三个 out 指针的可写性由调用方保证（C ABI 契约）；unaligned
            // 写避免调用方指针未对齐 = UB。
            unsafe {
                core::ptr::write_unaligned(out_ptr, lease.as_ptr() as usize);
                core::ptr::write_unaligned(out_len, lease.len());
                core::ptr::write_unaligned(out_device_addr, lease.device_addr() as u64);
            }
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 主动释放 DMA authority。backing lease 进 Core 私有 QUARANTINE（**不 free**，
/// 设备可能仍在 DMA）。返回 0 / `-Errno`。
extern "C" fn kcore_dma_release(handle: u64) -> i32 {
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    status(dma::release(&ctx, dma::DmaHandle::from_raw(handle)))
}

// ---------------------------------------------------------------------------
// Category 8（续）：IRQ authority（v3 起步；claim → register → enable）
// ---------------------------------------------------------------------------

/// 从 caller **已持有的 MMIO root** 派生同一台设备的中断线（v2）。
///
/// 语义：Core 校验 `mmio_handle`（slot/generation/owner/生命周期）→ 推出该设备的
/// `device_index` → 取设备记录的 `irq`（PLIC global interrupt id）→ authorize
/// （phase 1 恒 allow）→ 独占检查（该中断号已被认领则拒绝）→ grant `IrqHandle`。
/// IRQ **不再**按 compatible 独立匹配，因此不可能与 MMIO 认领到不同设备。
///
/// 链接名沿用原名，签名直接替换（由 `(compatible)` 变为 `(mmio_handle)`）。
///
/// 成功 = 0，raw handle 写入 `*out_handle`（同 `kcore_mmio_claim` 的编码：
/// 高 32 位 slot、低 32 位 generation；**不是中断号**）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller 或 caller 已
/// `Failed` / `EBADF`/`ESTALE`/`EACCES`/`EKEYREVOKED` MMIO handle 无效 /
/// `ENODEV` 设备无中断线 / `EBUSY` 中断线已被认领）。
extern "C" fn kcore_irq_claim(mmio_handle: u64, out_handle: *mut u64) -> i32 {
    if out_handle.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(ctx) = RequestContext::ambient() else {
        return Errno::EPERM.code();
    };
    if let Some(denied) = deny_if_failed(ctx.component) {
        return denied;
    }
    match irq::claim_derived(ctx.component, mmio::MmioHandle::from_raw(mmio_handle)) {
        Ok(handle) => {
            // SAFETY: 同 `kcore_mmio_claim`（调用方保证可写；unaligned 写防未对齐 UB）。
            unsafe { core::ptr::write_unaligned(out_handle, handle.to_raw()) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 主动释放一条 IRQ authority：**真的关断投递**（撤销 slot + 关断控制器线）。
///
/// 返回 0 / `-Errno`（`EPERM` 无法解析 caller / handle 类错误同 MMIO）。
extern "C" fn kcore_irq_release(handle: u64) -> i32 {
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    status(irq::release(caller, irq::IrqHandle::from_raw(handle)))
}

/// 注册该 IRQ 线的投递目标（组件处理函数 + opaque context，C6 骨架）。
///
/// `handler` 是组件提供的 `extern "C" fn(ctx: *mut ())`；`ctx` 原样回传，
/// Core 不解引用（同 interface vtable `ctx` 的生命周期契约）。delivery 存在
/// IRQ 表的 slot 里，**随 revoke/release 一起消失**——组件失败/卸载后不会再
/// 有回调进它的代码。
/// 成功 = 0；失败 = `-Errno`（`EPERM` 无法解析 caller / `EBADF` / `ESTALE` /
/// `EACCES` 非 owner / `EKEYREVOKED` / `EALREADY`）。
extern "C" fn kcore_irq_register(handle: u64, handler: irq::IrqHandler, ctx: *mut ()) -> i32 {
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    let delivery = irq::IrqDelivery::new(handler, ctx);
    let mut table = irq::get_table().lock();
    match table.set_delivery(caller, irq::IrqHandle::from_raw(handle), delivery) {
        Ok(()) => 0,
        Err(error) => Errno::from(error).code(),
    }
}

/// 使能该 IRQ 线：Core 验证 handle + delivery 后，才去配置中断控制器
/// （PLIC enable；C6 骨架）。
/// 成功 = 0；失败 = `-Errno`（`EPERM` 无法解析 caller / handle 类错误 /
/// `EINVAL` 尚未注册处理函数）。
extern "C" fn kcore_irq_enable(handle: u64) -> i32 {
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    match irq::enable(caller, irq::IrqHandle::from_raw(handle)) {
        Ok(()) => 0,
        Err(error) => Errno::from(error).code(),
    }
}

/// 把该 IRQ 线切成**轮询投递**（`delivery = Some(Polled)`，不装回调）。
///
/// Polled 线的事件由 Core 在顶半部计数并（首事件）掩蔽；owner 任务用
/// `kcore_irq_poll` 读计数、处理设备后用 `kcore_irq_ack` 确认。组件拿不到
/// 中断号，也拿不到掩蔽/放行中断的 authority——那些都在 Core。
/// 成功 = 0；失败 = `-Errno`（`EPERM` 无法解析 caller / handle 类错误）。
extern "C" fn kcore_irq_register_polled(handle: u64) -> i32 {
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    let mut table = irq::get_table().lock();
    match table.set_polled(caller, irq::IrqHandle::from_raw(handle)) {
        Ok(()) => 0,
        Err(error) => Errno::from(error).code(),
    }
}

/// 读取该 Polled 线累计的事件数（不清零；驱动按 delta 判断新事件）。
///
/// 成功 = 0，计数写入 `*out_count`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller /
/// handle 类错误 / `EINVAL` 该线不是 Polled）。
extern "C" fn kcore_irq_poll(handle: u64, out_count: *mut u64) -> i32 {
    if out_count.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    let table = irq::get_table().lock();
    match table.poll(caller, irq::IrqHandle::from_raw(handle)) {
        Ok(count) => {
            // SAFETY: 同 `kcore_mmio_claim`（调用方保证可写；unaligned 写防未对齐 UB）。
            unsafe { core::ptr::write_unaligned(out_count, count) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

/// 确认该 Polled 线的事件：清 `masked`，Core 在**锁外**重新放行该线
/// （`InterruptImpl::enable`）——这是软件掩蔽协议的闭环（见 `irq::on_external`）。
///
/// 成功 = 0；失败 = `-Errno`（`EPERM` 无法解析 caller / handle 类错误 /
/// `EINVAL` 该线不是 Polled）。
extern "C" fn kcore_irq_ack(handle: u64) -> i32 {
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    let number = {
        let mut table = irq::get_table().lock();
        match table.ack(caller, irq::IrqHandle::from_raw(handle)) {
            Ok(number) => number,
            Err(error) => return Errno::from(error).code(),
        }
    };
    // 锁外放行：trap 可重入、MMIO 慢（同 `complete` 的纪律）。
    InterruptImpl::enable(number);
    0
}

// ---------------------------------------------------------------------------
// 导出表（v1 白名单；添加符号 = 破坏性 ABI 变更，必须同步 bump 文档）
// ---------------------------------------------------------------------------

static EXPORTS: [Export; 43] = [
    // Category 0：Trace / 时钟（只读观察面）
    Export {
        name: b"kcore_trace_read",
        address: ExportAddress(kcore_trace_read as *const ()),
    },
    Export {
        name: b"kcore_trace_stats",
        address: ExportAddress(kcore_trace_stats as *const ()),
    },
    Export {
        name: b"kcore_now",
        address: ExportAddress(kcore_now as *const ()),
    },
    Export {
        name: b"kcore_timebase_hz",
        address: ExportAddress(kcore_timebase_hz as *const ()),
    },
    // Category 1：Runtime / shared heap
    Export {
        name: b"kcore_heap_alloc",
        address: ExportAddress(kcore_heap_alloc as *const ()),
    },
    Export {
        name: b"kcore_heap_dealloc",
        address: ExportAddress(kcore_heap_dealloc as *const ()),
    },
    // Category 2：Logging / diagnostics
    Export {
        name: b"kcore_console_write_byte",
        address: ExportAddress(kcore_console_write_byte as *const ()),
    },
    Export {
        name: b"kcore_log_line",
        address: ExportAddress(kcore_log_line as *const ()),
    },
    // Category 3：Machine query
    Export {
        name: b"kcore_machine_boot_hart",
        address: ExportAddress(kcore_machine_boot_hart as *const ()),
    },
    Export {
        name: b"kcore_machine_cpu_count",
        address: ExportAddress(kcore_machine_cpu_count as *const ()),
    },
    Export {
        name: b"kcore_machine_has_hart",
        address: ExportAddress(kcore_machine_has_hart as *const ()),
    },
    // Category 4：System query
    Export {
        name: b"kcore_free_page_count",
        address: ExportAddress(kcore_free_page_count as *const ()),
    },
    Export {
        name: b"kcore_task_count",
        address: ExportAddress(kcore_task_count as *const ()),
    },
    Export {
        name: b"kcore_component_count",
        address: ExportAddress(kcore_component_count as *const ()),
    },
    // Category 5：Component lifecycle（v2）
    Export {
        name: b"kcore_component_create",
        address: ExportAddress(kcore_component_create as *const ()),
    },
    Export {
        name: b"kcore_component_load",
        address: ExportAddress(kcore_component_load as *const ()),
    },
    Export {
        name: b"kcore_interface_publish",
        address: ExportAddress(kcore_interface_publish as *const ()),
    },
    Export {
        name: b"kcore_interface_available",
        address: ExportAddress(kcore_interface_available as *const ()),
    },
    Export {
        name: b"kcore_interface_bind",
        address: ExportAddress(kcore_interface_bind as *const ()),
    },
    Export {
        name: b"kcore_interface_refresh",
        address: ExportAddress(kcore_interface_refresh as *const ()),
    },
    // Category 6：Task control（v2）
    Export {
        name: b"kcore_task_create",
        address: ExportAddress(kcore_task_create as *const ()),
    },
    Export {
        name: b"kcore_task_start",
        address: ExportAddress(kcore_task_start as *const ()),
    },
    Export {
        name: b"kcore_task_yield",
        address: ExportAddress(kcore_task_yield as *const ()),
    },
    Export {
        name: b"kcore_task_exit",
        address: ExportAddress(kcore_task_exit as *const ()),
    },
    Export {
        name: b"kcore_task_state",
        address: ExportAddress(kcore_task_state as *const ()),
    },
    // Category 6（续）：Panic containment（v2）
    Export {
        name: b"kcore_panic_escape",
        address: ExportAddress(kcore_panic_escape as *const ()),
    },
    // Category 7：Scheduler（v2）
    Export {
        name: b"kcore_sched_run",
        address: ExportAddress(kcore_sched_run as *const ()),
    },
    // Category 8：Resource authority（v3 起步）
    Export {
        name: b"kcore_device_nth",
        address: ExportAddress(kcore_device_nth as *const ()),
    },
    Export {
        name: b"kcore_mmio_claim",
        address: ExportAddress(kcore_mmio_claim as *const ()),
    },
    Export {
        name: b"kcore_mmio_read_u32",
        address: ExportAddress(kcore_mmio_read_u32 as *const ()),
    },
    Export {
        name: b"kcore_mmio_write_u32",
        address: ExportAddress(kcore_mmio_write_u32 as *const ()),
    },
    Export {
        name: b"kcore_mmio_release",
        address: ExportAddress(kcore_mmio_release as *const ()),
    },
    Export {
        name: b"kcore_mmio_lease",
        address: ExportAddress(kcore_mmio_lease as *const ()),
    },
    // Category 8（续）：DMA authority（v3 起步）
    Export {
        name: b"kcore_dma_alloc",
        address: ExportAddress(kcore_dma_alloc as *const ()),
    },
    Export {
        name: b"kcore_dma_lease",
        address: ExportAddress(kcore_dma_lease as *const ()),
    },
    Export {
        name: b"kcore_dma_release",
        address: ExportAddress(kcore_dma_release as *const ()),
    },
    // Category 8（续）：IRQ authority（v3 起步）
    Export {
        name: b"kcore_irq_claim",
        address: ExportAddress(kcore_irq_claim as *const ()),
    },
    Export {
        name: b"kcore_irq_register",
        address: ExportAddress(kcore_irq_register as *const ()),
    },
    Export {
        name: b"kcore_irq_enable",
        address: ExportAddress(kcore_irq_enable as *const ()),
    },
    Export {
        name: b"kcore_irq_register_polled",
        address: ExportAddress(kcore_irq_register_polled as *const ()),
    },
    Export {
        name: b"kcore_irq_poll",
        address: ExportAddress(kcore_irq_poll as *const ()),
    },
    Export {
        name: b"kcore_irq_ack",
        address: ExportAddress(kcore_irq_ack as *const ()),
    },
    Export {
        name: b"kcore_irq_release",
        address: ExportAddress(kcore_irq_release as *const ()),
    },
];

/// 按未 mangled 字节名精确查找导出地址（线性扫：条目少，不值得排序/哈希）。
/// 返回的内核地址由 loader 作为 ELF 重定位的 `S` 使用。
pub fn resolve(name: &[u8]) -> Option<usize> {
    EXPORTS
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.address.0 as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_all_entries() {
        use alloc::string::String;
        for name in [
            &b"kcore_trace_read"[..],
            &b"kcore_trace_stats"[..],
            &b"kcore_heap_alloc"[..],
            &b"kcore_heap_dealloc"[..],
            &b"kcore_console_write_byte"[..],
            &b"kcore_log_line"[..],
            &b"kcore_machine_boot_hart"[..],
            &b"kcore_machine_cpu_count"[..],
            &b"kcore_machine_has_hart"[..],
            &b"kcore_free_page_count"[..],
            &b"kcore_task_count"[..],
            &b"kcore_component_count"[..],
            &b"kcore_component_create"[..],
            &b"kcore_component_load"[..],
            &b"kcore_interface_publish"[..],
            &b"kcore_interface_available"[..],
            &b"kcore_interface_bind"[..],
            &b"kcore_interface_refresh"[..],
            &b"kcore_task_create"[..],
            &b"kcore_task_start"[..],
            &b"kcore_task_yield"[..],
            &b"kcore_task_exit"[..],
            &b"kcore_task_state"[..],
            &b"kcore_panic_escape"[..],
            &b"kcore_sched_run"[..],
            &b"kcore_device_nth"[..],
            &b"kcore_mmio_claim"[..],
            &b"kcore_mmio_read_u32"[..],
            &b"kcore_mmio_write_u32"[..],
            &b"kcore_mmio_release"[..],
            &b"kcore_mmio_lease"[..],
            &b"kcore_dma_alloc"[..],
            &b"kcore_dma_lease"[..],
            &b"kcore_dma_release"[..],
            &b"kcore_irq_claim"[..],
            &b"kcore_irq_register"[..],
            &b"kcore_irq_enable"[..],
            &b"kcore_irq_register_polled"[..],
            &b"kcore_irq_poll"[..],
            &b"kcore_irq_ack"[..],
            &b"kcore_irq_release"[..],
        ] {
            assert!(resolve(name).is_some(), "{}", String::from_utf8_lossy(name));
        }
    }

    #[test]
    fn rejects_unknown_names() {
        assert_eq!(resolve(b"kcore_frame_alloc"), None);
        assert_eq!(resolve(b"kcore_alloc_region"), None);
        assert_eq!(resolve(b"kcore_address_space_map"), None);
        assert_eq!(resolve(b"kcore_task_table_create"), None);
        assert_eq!(resolve(b"kcore_registry_declare"), None);
        assert_eq!(resolve(b"kcore_context_switch"), None);
        assert_eq!(resolve(b"kcore_mmio_grant"), None, "设备认领只能走 claim");
        assert_eq!(resolve(b"kcore_"), None);
        assert_eq!(resolve(b""), None);
    }

    #[test]
    fn names_are_exact_not_prefix() {
        assert!(resolve(b"kcore_log_line2").is_none(), "禁止前缀匹配");
        assert!(resolve(b"x?kcore_log_line").is_none(), "禁止后缀匹配");
    }

    /// DMA 方向 ABI 编码锚定（`docs/driver-model.md` §6.2）：`as_i32` 与解码
    /// `dma_direction_from_i32` 必须互为逆，且值固定为 0/1/2；`kcomp-sdk` 的镜像
    /// 枚举同值（SDK 侧有对应测试 `dma_direction_encoding_is_stable`）。
    #[test]
    fn dma_direction_encoding_is_stable() {
        use crate::handle::dma::DmaDirection::{Bidirectional, FromDevice, ToDevice};
        assert_eq!(ToDevice.as_i32(), 0);
        assert_eq!(FromDevice.as_i32(), 1);
        assert_eq!(Bidirectional.as_i32(), 2);
        for direction in [ToDevice, FromDevice, Bidirectional] {
            assert_eq!(dma_direction_from_i32(direction.as_i32()), Some(direction));
        }
        assert_eq!(dma_direction_from_i32(3), None, "越界编码必须被拒绝");
    }

    /// `kcore_panic_escape`：host 无活动 containment 边界时安全地返回 `-EPERM`
    /// （不触发 context switch）。活动边界下永不返回，只能由 QEMU/ArchTest 验证。
    #[test]
    fn panic_escape_without_boundary_returns_eperm() {
        // 其它测试会短暂安装 task guard；仅在确认无活动边界时断言，避免把并行
        // 测试的 guard 当成真实边界而触发一次 context switch。
        if crate::component::containment::active_escape().is_some() {
            return;
        }
        let escape = resolve(b"kcore_panic_escape").expect("panic escape export registered");
        let escape: extern "C" fn() -> i32 = unsafe { core::mem::transmute(escape) };
        assert_eq!(escape(), Errno::EPERM.code());
    }

    /// v3 资源 authority API 的错误约定：`0 / -Errno`，值走 out 参数。
    #[test]
    fn resource_authority_apis_follow_status_convention() {
        // `kcore_device_nth`：null out → EFAULT；compatible 非法 → EINVAL。
        let device_nth = resolve(b"kcore_device_nth").unwrap();
        let device_nth: extern "C" fn(*const u8, usize, u32, *mut u32) -> i32 =
            unsafe { core::mem::transmute(device_nth) };
        let mut device_id = 0u32;
        assert_eq!(
            device_nth(b"virtio,mmio".as_ptr(), 11, 0, core::ptr::null_mut()),
            -14
        );
        assert_eq!(device_nth(core::ptr::null(), 0, 0, &mut device_id), -22);

        let claim = resolve(b"kcore_mmio_claim").unwrap();
        let claim: extern "C" fn(u32, *mut u64) -> i32 = unsafe { core::mem::transmute(claim) };
        let read = resolve(b"kcore_mmio_read_u32").unwrap();
        let read: extern "C" fn(u64, u32, *mut u32) -> i32 = unsafe { core::mem::transmute(read) };
        let _write: extern "C" fn(u64, u32, u32) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_mmio_write_u32").unwrap()) };
        let _release: extern "C" fn(u64) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_mmio_release").unwrap()) };

        let mut out = 0u64;
        // out 为空 → EFAULT（早于设备/硬件逻辑，host 可安全断言）
        assert_eq!(claim(0, core::ptr::null_mut()), -14);
        assert_eq!(read(0, 0, core::ptr::null_mut()), -14);

        // mmio lease：两个 out 任一为空 → EFAULT（早于 handle/caller 解析）
        let lease: extern "C" fn(u64, *mut usize, *mut usize) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_mmio_lease").unwrap()) };
        let (mut lease_ptr, mut lease_len) = (0usize, 0usize);
        assert_eq!(lease(0, core::ptr::null_mut(), &mut lease_len), -14);
        assert_eq!(lease(0, &mut lease_ptr, core::ptr::null_mut()), -14);

        // IRQ claim：从 MMIO handle 派生；out 为空 → EFAULT（早于 caller 解析）。
        let irq_claim = resolve(b"kcore_irq_claim").unwrap();
        let irq_claim: extern "C" fn(u64, *mut u64) -> i32 =
            unsafe { core::mem::transmute(irq_claim) };
        assert_eq!(irq_claim(0, core::ptr::null_mut()), -14);

        // IRQ release：host 无 caller → EPERM（不是 panic）。
        let _irq_release: extern "C" fn(u64) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_irq_release").unwrap()) };

        // IRQ poll：out 为空 → EFAULT（早于 handle/caller 解析）
        let irq_poll: extern "C" fn(u64, *mut u64) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_irq_poll").unwrap()) };
        assert_eq!(irq_poll(0, core::ptr::null_mut()), -14);

        // DMA alloc：out 为空 → EFAULT；direction 非法 → EINVAL（都早于 caller 解析）
        let dma_alloc: extern "C" fn(u64, usize, i32, *mut u64) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_dma_alloc").unwrap()) };
        assert_eq!(dma_alloc(0, 4096, 0, core::ptr::null_mut()), -14);
        assert_eq!(dma_alloc(0, 4096, 3, &mut out), -22);

        // DMA lease：三个 out 任一为空 → EFAULT（早于 handle/caller 解析）
        let dma_lease: extern "C" fn(u64, *mut usize, *mut usize, *mut u64) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_dma_lease").unwrap()) };
        let (mut dma_ptr, mut dma_len, mut dma_addr) = (0usize, 0usize, 0u64);
        assert_eq!(
            dma_lease(0, core::ptr::null_mut(), &mut dma_len, &mut dma_addr),
            -14
        );
        assert_eq!(
            dma_lease(0, &mut dma_ptr, core::ptr::null_mut(), &mut dma_addr),
            -14
        );
        assert_eq!(
            dma_lease(0, &mut dma_ptr, &mut dma_len, core::ptr::null_mut()),
            -14
        );

        // DMA release 同形：host 无 caller → EPERM（不是 panic）
        let _dma_release: extern "C" fn(u64) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_dma_release").unwrap()) };
    }

    /// Failed 实例门禁：获取 authority / 创建 work 的入口一律 `-EPERM`，但已持有
    /// handle 的 `release` 仍可用（teardown 不被门禁挡住）。
    #[test]
    fn failed_component_is_denied_new_authority_but_may_release() {
        use crate::component::{containment, registry};

        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        crate::handle::init();
        let _boundary = containment::test_boundary_lock();

        // Given：一个被标记 Failed 的组件。
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(crate::component::image::ComponentImageId::from_raw(1))
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.mark_failed(id).unwrap();
            id
        };

        // When / Then：身份解析到该 Failed 组件（create 边界）；acquiring 入口
        // 全部 `-EPERM`，但 release 一个已持有的 handle 仍然成功。
        containment::with_test_init_boundary(Some(id), || {
            let mut out = 0u64;
            let mut out_task = 0u32;
            assert_eq!(kcore_mmio_claim(0, &mut out), Errno::EPERM.code());
            assert_eq!(kcore_irq_claim(0, &mut out), Errno::EPERM.code());
            assert_eq!(kcore_dma_alloc(0, 4096, 0, &mut out), Errno::EPERM.code());
            assert_eq!(
                kcore_task_create(0x1000, core::ptr::null_mut(), &mut out_task),
                Errno::EPERM.code()
            );
            assert_eq!(
                kcore_interface_publish(
                    b"svc".as_ptr(),
                    3,
                    1,
                    0xAB,
                    core::ptr::null(),
                    core::ptr::null_mut(),
                ),
                Errno::EPERM.code()
            );

            let granted = mmio::get_table().lock().grant(
                id,
                mmio::MmioRegion {
                    base: 0,
                    size: 0x1000,
                    device_index: 240,
                },
            );
            assert_eq!(kcore_mmio_release(granted.to_raw()), 0);
        });
    }

    /// 锁定 `deny_if_failed` 的**已知范围**：只拒绝 `Failed`（逻辑死亡）实例；
    /// 已 `Stopped` 的实例**不在**其内。
    ///
    /// review `docs/resource-model-review.md` §C.5 明确记录这是 known gap：
    /// 其它"不应获得新权威"的状态（`Stopping` / `Stopped`）未被该门禁覆盖。
    /// 本步骤只 documenting + 锁定现状，**不修改行为**。
    #[test]
    fn deny_if_failed_denies_failed_only_not_stopped() {
        use crate::component::registry;

        registry::init();

        let image = crate::component::image::ComponentImageId::from_raw(9001);
        let failed = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(image).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.mark_failed(id).unwrap();
            id
        };
        let stopped = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(image).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            reg.begin_stop(id).unwrap();
            reg.finish_stop(id).unwrap();
            id
        };

        assert_eq!(
            deny_if_failed(failed),
            Some(Errno::EPERM.code()),
            "Failed 实例必须被获取权威门禁拒绝"
        );
        assert_eq!(
            deny_if_failed(stopped),
            None,
            "Stopped 实例当前不被 deny_if_failed 拒绝（已知 gap，仅锁定现状）"
        );
    }

    /// 接口 ABI（exact fingerprint）：bind/refresh 的早期错误约定
    /// （`EFAULT` out 为空 / `EINVAL` 名字非法），host 可在不触碰 registry 前断言。
    #[test]
    fn interface_bind_and_refresh_reject_null_outputs() {
        let bind = resolve(b"kcore_interface_bind").unwrap();
        let bind: extern "C" fn(
            *const u8,
            usize,
            u32,
            u64,
            *mut u64,
            *mut usize,
            *mut usize,
            *mut u64,
        ) -> i32 = unsafe { core::mem::transmute(bind) };
        let (mut b, mut api, mut ctx, mut generation) = (0u64, 0usize, 0usize, 0u64);
        // 任一 out 为空 → EFAULT（早于 registry 解析）
        assert_eq!(
            bind(
                b"x".as_ptr(),
                1,
                1,
                0,
                core::ptr::null_mut(),
                &mut api,
                &mut ctx,
                &mut generation
            ),
            -14
        );
        assert_eq!(
            bind(
                b"x".as_ptr(),
                1,
                1,
                0,
                &mut b,
                core::ptr::null_mut(),
                &mut ctx,
                &mut generation
            ),
            -14
        );
        assert_eq!(
            bind(
                b"x".as_ptr(),
                1,
                1,
                0,
                &mut b,
                &mut api,
                core::ptr::null_mut(),
                &mut generation
            ),
            -14
        );
        assert_eq!(
            bind(
                b"x".as_ptr(),
                1,
                1,
                0,
                &mut b,
                &mut api,
                &mut ctx,
                core::ptr::null_mut()
            ),
            -14
        );
        // 名字非法 → EINVAL（早于 registry 解析）
        assert_eq!(
            bind(
                core::ptr::null(),
                0,
                1,
                0,
                &mut b,
                &mut api,
                &mut ctx,
                &mut generation
            ),
            -22
        );

        let refresh = resolve(b"kcore_interface_refresh").unwrap();
        let refresh: extern "C" fn(u64, u64, *mut usize, *mut usize, *mut u64) -> i32 =
            unsafe { core::mem::transmute(refresh) };
        assert_eq!(
            refresh(0, 0, core::ptr::null_mut(), &mut ctx, &mut generation),
            -14
        );
        assert_eq!(
            refresh(0, 0, &mut api, core::ptr::null_mut(), &mut generation),
            -14
        );
        assert_eq!(
            refresh(0, 0, &mut api, &mut ctx, core::ptr::null_mut()),
            -14
        );
    }

    #[test]
    fn heap_alloc_dealloc_roundtrip() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let alloc = resolve(b"kcore_heap_alloc").unwrap();
        let dealloc = resolve(b"kcore_heap_dealloc").unwrap();
        let alloc: extern "C" fn(usize, usize) -> *mut u8 = unsafe { core::mem::transmute(alloc) };
        let dealloc: extern "C" fn(*mut u8, usize, usize) -> i32 =
            unsafe { core::mem::transmute(dealloc) };

        let p = alloc(32, 8);
        assert!(!p.is_null(), "共享堆必须能分配");
        // 写读往返，验证可写
        unsafe {
            core::ptr::write_volatile(p as *mut u64, 0xDEAD_BEEF);
            assert_eq!(core::ptr::read_volatile(p as *mut u64), 0xDEAD_BEEF);
        }
        assert_eq!(dealloc(p, 32, 8), 0);
    }

    #[test]
    fn heap_alloc_invalid_layout_returns_null() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let alloc = resolve(b"kcore_heap_alloc").unwrap();
        let alloc: extern "C" fn(usize, usize) -> *mut u8 = unsafe { core::mem::transmute(alloc) };
        // size=0 与非法 align（非 2 的幂）必须返回 null，不得 panic/UB。
        assert!(alloc(0, 8).is_null());
        assert!(alloc(16, 3).is_null());
    }

    /// `kcore_trace_read`：一次只读一条、`out_next` 作续读游标、读空返回
    /// `ENOENT`、空指针返回 `EFAULT`。走的是**真实导出函数**，不是复刻逻辑。
    ///
    /// 只在 `CONFIG_TRACE=y` 时有意义：trace 编译掉后没有可读的事件。
    #[test]
    #[cfg(feature = "trace")]
    fn trace_read_export_returns_one_record_and_a_cursor() {
        let _serial = crate::trace::test_support::GUARD.lock();
        crate::trace::reset_for_test();
        crate::trace::emit(crate::trace::TraceEvent::IrqEnter { irq: 7 });
        crate::trace::emit(crate::trace::TraceEvent::IrqAck { irq: 7 });

        let mut record = crate::trace::TraceRecordAbi {
            seq: 0,
            timestamp: 0,
            kind: 0,
            flags: 0,
            a: 0,
            b: 0,
            c: 0,
        };
        let mut next = 0u64;

        // 第一条：IrqEnter，游标推进到 seq + 1。
        assert_eq!(kcore_trace_read(0, &mut record, &mut next), 0);
        assert_eq!(record.kind, crate::trace::abi::KIND_IRQ_ENTER);
        assert_eq!(record.a, 7, "IrqEnter 的 irq 走 payload a");
        assert_eq!(next, record.seq + 1);

        // 用游标续读第二条。
        assert_eq!(kcore_trace_read(next, &mut record, &mut next), 0);
        assert_eq!(record.kind, crate::trace::abi::KIND_IRQ_ACK);

        // 读完必须返回 ENOENT —— 不能返回 0，否则无法区分"读到"与"读完"。
        assert_eq!(
            kcore_trace_read(next, &mut record, &mut next),
            Errno::ENOENT.code()
        );

        // 空指针 → EFAULT（两个 out 参数都要校验）。
        assert_eq!(
            kcore_trace_read(0, core::ptr::null_mut(), &mut next),
            Errno::EFAULT.code()
        );
        assert_eq!(
            kcore_trace_read(0, &mut record, core::ptr::null_mut()),
            Errno::EFAULT.code()
        );
    }

    /// `kcore_trace_stats`：字段显式编码、空指针 `EFAULT`，走真实导出函数。
    ///
    /// 只在 `CONFIG_TRACE=y` 时有意义：trace 编译掉后 ring 恒空（`enabled_mask == 0`）。
    #[test]
    #[cfg(feature = "trace")]
    fn trace_stats_export_reports_ring_state() {
        let _serial = crate::trace::test_support::GUARD.lock();
        crate::trace::reset_for_test();
        crate::trace::emit(crate::trace::TraceEvent::IrqEnter { irq: 7 });
        crate::trace::emit(crate::trace::TraceEvent::IrqAck { irq: 7 });

        let mut out = crate::trace::TraceStatsAbi {
            capacity: 0,
            oldest_seq: 0,
            next_seq: 0,
            overwritten_total: 0,
            enabled_mask: 0,
        };
        assert_eq!(kcore_trace_stats(&mut out), 0);
        assert_eq!(out.capacity, crate::trace::capacity() as u64);
        assert_eq!(out.oldest_seq, 1);
        assert_eq!(out.next_seq, 3);
        assert_eq!(out.overwritten_total, 0);
        assert_eq!(out.enabled_mask, crate::trace::ENABLED_MASK_ALL);

        assert_eq!(
            kcore_trace_stats(core::ptr::null_mut()),
            Errno::EFAULT.code()
        );
    }
}
