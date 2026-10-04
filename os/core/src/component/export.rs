//! 组件 → Core 稳定 API（EXPORT_SYMBOL 教学版）。
//!
//! 符号清单与签名以 `abi/core.toml` 为唯一来源（生成物在 `generated/`；文档见
//! `docs/modules/core/component.md`）。本文件只放边界纪律：
//!
//! - **白名单即契约**：表内条目锁定（名字 + C ABI 签名）；未导出符号 →
//!   loader `UnresolvedSymbol`，整次加载失败（exact-name resolution，不建 flat
//!   symbol 表）。组件侧用 `unsafe extern "C" { #[link_name = "kcore_..."] ... }`
//!   声明，loader 按未 mangled 字节名精确匹配。
//! - **返回值形状**（按能否失败分类）：可失败、无值 → `i32 status`
//!   （`0` / `-Errno`）；可失败、有值 → `i32 status + out 参数`（值不混进返回
//!   值）；不会失败（纯 query）→ 直接返回值，`0` 是普通值不是哨兵。
//! - **宽度规则**：`usize` 只用于"语义就是指针宽"的量（地址 `entry`、
//!   `(ptr, len)` 长度、分配器 `size/align`）；counts/ids → `u32`；布尔/编码 →
//!   `i32`；DMA mapping identity → `u64`，只经 `status + out` 回传。
//! - `Errno` 是稳定、Linux/POSIX 风格的数值命名空间（`os/core/src/errno.rs`）；
//!   各子系统的内部错误（`TaskError` / `ComponentLoadError` / `SchedError` /
//!   `EndpointError` / `DeviceClaimError` / `IrqError` / `DmaError`）保持各自
//!   为政，只在导出边界翻译成 `Errno`。
//!
//! # Core-critical 导出体
//!
//! 每个普通 `kcore_*` 导出体都包在 `with_core_critical` 里：导出体是跑在组件
//! 栈上的 **Core 代码**（可能持有 Core 锁），期间的 panic 是 **Core panic**，
//! containment 必须拒绝逃逸（否则 Core bug 会被误算成边界 owner 的失败，且恢复
//! 路径会带着 Core 锁进入 `fail_component` → 死锁）。组件代码在边界安装时把
//! 深度挂起到 0，所以组件自己的 panic 照常可逃逸（见 `containment.rs` 的
//! "Core ABI depth" 与 `containment::panic_escape`）。**唯一例外**是 SDK 的显式
//! 逃逸请求 `kcore_panic_escape`：包了它，逃逸请求本身就永远不可逃逸。
//!
//! # 身份解析与 Failed 门禁
//!
//! - **principal = 最内层当前活动的 Core-managed 执行边界**
//!   （`containment::active_escape`）：组件任务 → task owner；
//!   `kcomp_instance_create`（含嵌套创建）→ 被创建的实例；service call
//!   （`kcore_endpoint_call`）→ **provider**（caller task 只作执行来源）；
//!   嵌套 create / service call 返回或 panic 后恢复上一层边界。所有 authority /
//!   task / endpoint 入口统一走 `RequestContext::ambient()` / `ambient_init()`，
//!   不再各自偏好当前任务 owner。
//! - **Failed 实例门禁**：获取资源 / 创建 work 的入口（`kcore_device_claim`、
//!   `kcore_irq_register`、`kcore_dma_alloc`、`kcore_dma_map`、`kcore_task_create`、
//!   `kcore_endpoint_publish`、`kcore_endpoint_call`）在 caller/provider 已
//!   `Failed` 时返回 `-EPERM`；`release` / `revoke` 及已持有资源（按 DeviceId
//!   锚定）的 teardown 操作**不受此门禁限制**。
//!
//! # 明确不导出（未经 Core validation 的裸 authority mutation）
//!
//! 组件可以 **request** 资源（discover + request → Core 记 owner → 返回可访问
//! 窗口），但任何 Core truth 的 mutation 都必须由 Core 验证后提交并留 trace；
//! 裸 mutation 入口一律不导出：物理帧（`memory::alloc_region` /
//! `free_region` / `vm_page_alloc`——组件走 `kcore_memory_acquire`）、地址空间
//! （`KernelAddressSpace::map/unmap/activate`——需要 `AddressSpaceHandle`）、
//! 裸任务表（`TaskTable::create` / `set_task_state` / context switch；`kcore_task_*`
//! 是带验证的语义入口）、registry 生命周期（`registry::declare/start/unload`）、
//! Core authoritative trace event。

use crate::component::ComponentId;
use crate::component::abi::{InterfaceAbi, InterfaceKind};
use crate::component::call;
use crate::component::containment::{KcompCreateArgs, with_core_critical};
use crate::component::endpoint::{self, ContractId, EndpointId, ExecutionDomain};
use crate::component::registry;
use crate::errno::{Errno, status};
use crate::generated::abi::{KCORE_MEMORY_VIEW_LOCAL_VA, MemoryView};
use crate::machine;
use crate::memory;
use crate::resource::{RequestContext, device, dma, irq};
use crate::sched;
use crate::task::{self, TaskId, TaskState};
use arch::{Console, ConsoleImpl};
use core::alloc::GlobalAlloc;

// ---------------------------------------------------------------------------
// 导出表（v1 白名单；添加符号 = 破坏性 ABI 变更，必须同步 bump 文档）
// ---------------------------------------------------------------------------
// 表本体由 tools/kabi/kabi_gen.py 从 `abi/core.toml` 生成到
// `generated/exports.rs`：typed 引用锚定实现签名（缺实现 / 签名变了 = 编译错误）。
#[path = "generated/exports.rs"]
mod exports;

use exports::EXPORTS;

#[path = "export/query.rs"]
mod query;
use query::*;

#[path = "export/user.rs"]
mod user;
use user::*;

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
// Category 1：Memory resource（域视图，无账本）
// ---------------------------------------------------------------------------

/// 取一段内存 backing，返回**本执行域访问窗口**（`kcore_memory_view`）。
///
/// **无账本**：Core 不为 region 建记录、不发 id、不记 owner——`view` 自身
/// （`base` / `len`）就是身份，释放凭同一个 view 走 [`kcore_memory_release`]。
/// `min_len > 0`、`min_align` 为非零 2 的幂；成功时 `view.len >= min_len`
/// （实际 backing 粒度 = Core 页，今天 4 KiB）。首次交付**零初始化**。
///
/// 成功 = 0，view 写入 `*out_view`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EINVAL` size/align 非法 /
/// `EOVERFLOW` `min_len` 超出本域指针宽 / `ENOMEM` 物理内存耗尽）。
extern "C" fn kcore_memory_acquire(min_len: u64, min_align: u64, out_view: *mut MemoryView) -> i32 {
    with_core_critical(|| {
        if out_view.is_null() {
            return Errno::EFAULT.code();
        }
        if min_len == 0 || min_align == 0 || !min_align.is_power_of_two() {
            return Errno::EINVAL.code();
        }
        let Ok(size) = usize::try_from(min_len) else {
            return Errno::EOVERFLOW.code();
        };
        match memory::alloc_region(size) {
            Ok(lease) => {
                let base = lease.base();
                let len = lease.size();
                // 首次交付零初始化：backing 可能带着上一任占用者的内容。
                // SAFETY: base/len 来自 alloc_region，是有效的可写物理区域
                // （v1 identity 映射下 pa == va，与 vm_page_alloc 同一前提）。
                unsafe { core::ptr::write_bytes(base as *mut u8, 0, len) };
                // 故意泄漏 lease：本契约没有 Core 侧账本，backing 的释放凭 view
                // 走 `kcore_memory_release`——让 Drop 归还会与显式 release 双重释放。
                core::mem::forget(lease);
                // SAFETY: out_view 已校验非空；可写性由调用方保证（C ABI 契约）。
                unsafe {
                    out_view.write(MemoryView {
                        kind: KCORE_MEMORY_VIEW_LOCAL_VA,
                        reserved: 0,
                        base: base as u64,
                        len: len as u64,
                    });
                }
                0
            }
            Err(_) => Errno::ENOMEM.code(),
        }
    })
}

/// 交回一个 [`kcore_memory_acquire`] 交付的 view：把 backing 归还分配器。
///
/// KernelNative 是**受信操作**：不校验归属、无账本——只有 `(base, len)` 必须与
/// acquire 交付的 view 完全一致。
///
/// view 以 `*const` 传入（`memory-and-heap.md` §2 的 C 原型是 by-value 聚合）：
/// `kcomp_abi_drift.rs` 的 C 签名哨兵只按宽度分类参数、没有 by-value 聚合类别，
/// by-value 会打挂那个冻结测试；RISC-V psABI 下 24 字节聚合本就按引用传递，
/// wire ABI 等价。
///
/// 成功 = 0；失败 = `-Errno`（`EFAULT` 空指针 / `EINVAL` kind 非法 /
/// `reserved` 非 0 / `(base, len)` 不是一次 acquire 产物的形状）。
extern "C" fn kcore_memory_release(view: *const MemoryView) -> i32 {
    with_core_critical(|| {
        if view.is_null() {
            return Errno::EFAULT.code();
        }
        // SAFETY: 调用方保证 view 指向调用期间有效的 MemoryView（C ABI 契约）。
        let view = unsafe { *view };
        if view.kind != KCORE_MEMORY_VIEW_LOCAL_VA || view.reserved != 0 {
            return Errno::EINVAL.code();
        }
        let (Ok(base), Ok(len)) = (usize::try_from(view.base), usize::try_from(view.len)) else {
            return Errno::EINVAL.code();
        };
        match memory::free_region_raw(base, len) {
            Ok(()) => 0,
            Err(_) => Errno::EINVAL.code(),
        }
    })
}

// ---------------------------------------------------------------------------
// Category 1（续）：KernelNative heap backend（部署后端；非通用内存 ABI）
// ---------------------------------------------------------------------------
//
// KernelNative 组件与 Core 同特权、同地址空间，所以共享同一个
// `memory::KernelAllocator`——这是**部署形态决定的窄后端**，不是通用 / 跨域内存
// ABI：Isolated / Sandboxed 的装载显式拒绝这两个符号（它们在自己的可写 image 里
// 放私有分配器，backing 走 `kcore_memory_acquire/release`；见
// `docs/architecture/memory-and-heap.md` §6）。两个实现体只碰共享堆：不取
// registry / endpoint / task 锁，不打印（日志可能分配），不做 ownership / 记账 /
// 撤销。

/// KernelNative 部署后端：从 Core 共享堆分配（契约 = Rust `GlobalAlloc::alloc`）。
///
/// `size == 0`、非法 layout（`align` 非 2 的幂 / 为 0 / 溢出）或堆耗尽 → null
/// （**绝不 panic**）。成功返回的指针只能用 [`kcore_heap_dealloc`] 释放，且
/// `(size, align)` 必须与本次调用逐字一致（错配 = UB，与 C `malloc/free` 同类）。
extern "C" fn kcore_heap_alloc(size: usize, align: usize) -> *mut u8 {
    with_core_critical(|| {
        if size == 0 {
            return core::ptr::null_mut();
        }
        let Ok(layout) = core::alloc::Layout::from_size_align(size, align) else {
            return core::ptr::null_mut();
        };
        // SAFETY: layout 已由 from_size_align 校验（size > 0、align 为合法 2 的幂）；
        // KernelAllocator 是共享堆的 GlobalAlloc 实现，ptr/layout 匹配由调用方保证。
        unsafe { memory::KernelAllocator.alloc(layout) }
    })
}

/// 归还一次 [`kcore_heap_alloc`] 的分配（契约 = Rust `GlobalAlloc::dealloc`）。
///
/// `(ptr, size, align)` 必须与那次成功 alloc **逐字一致**：共享堆按 `Layout` 路由
/// slab / buddy，**不得**从取整后的容量反推。成功 = `0`；失败 = `-Errno`
/// （`EFAULT` 空指针 / `EINVAL` 非法或 `size == 0` 的 layout——没有一次成功 alloc
/// 会产出这种形状）。释放只回到共享堆：不撤销、不记账。
extern "C" fn kcore_heap_dealloc(ptr: *mut u8, size: usize, align: usize) -> i32 {
    with_core_critical(|| {
        if ptr.is_null() {
            return Errno::EFAULT.code();
        }
        if size == 0 {
            return Errno::EINVAL.code();
        }
        let Ok(layout) = core::alloc::Layout::from_size_align(size, align) else {
            return Errno::EINVAL.code();
        };
        // SAFETY: 调用方保证 ptr 来自一次成功的 kcore_heap_alloc 且 layout 逐字一致
        // （C ABI 契约，错配 = UB）。
        unsafe { memory::KernelAllocator.dealloc(ptr, layout) };
        0
    })
}

// ---------------------------------------------------------------------------
// Category 2：Logging / diagnostics
// ---------------------------------------------------------------------------

extern "C" fn kcore_console_write_byte(byte: u8) {
    with_core_critical(|| ConsoleImpl::write_byte(byte));
}

/// 输出一行（`[kcomp] ` 前缀）。返回 0 / `-Errno`（`EFAULT` 空指针 /
/// `EOVERFLOW` 长度超 `isize::MAX`）。
extern "C" fn kcore_log_line(ptr: *const u8, len: usize) -> i32 {
    with_core_critical(|| {
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
    })
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
    with_core_critical(|| {
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
    })
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
    with_core_critical(|| {
        if out.is_null() {
            return Errno::EFAULT.code();
        }
        let stats = crate::trace::stats();
        let abi = crate::trace::TraceStatsAbi::from(&stats);
        // SAFETY: out 在上面已校验非空；它是调用者提供的可写内存。
        unsafe { out.write(abi) };
        0
    })
}

// ---------------------------------------------------------------------------
// Category 3：Machine query（已提交机器真相的只读查询；counts/ids → u32）
// ---------------------------------------------------------------------------

// NOTE(SMP): 硬件身份已是 u64（`HardwareCpuId`）。这里暂时保留 u32 的组件 ABI
// 形状（QEMU hartid 都在 u32 内）；把 ABI 原地加宽到 u64 需要同步重生成
// abi/* 产物（`make abi-gen`），作为独立的组件边界任务。
//
// 转换一律**校验后**窄化（`try_from` + 明确的兜底值），不用 `as` 截断。
extern "C" fn kcore_machine_boot_hart() -> u32 {
    with_core_critical(|| {
        machine::committed().map_or(0, |m| u32::try_from(m.boot_hardware_id.raw()).unwrap_or(0))
    })
}

/// 单调时钟（`rdtime` 的原始 tick）——组件侧计时用，无 authority 语义。
///
/// 频率见 [`kcore_timebase_hz`]。注意真机上 timebase 常是 10 MHz（1 tick =
/// 100 ns），测很短的操作要么**累积多次再除**，要么等 cycle 源（`rdcycle`）。
extern "C" fn kcore_now() -> u64 {
    with_core_critical(<arch::TimerImpl as arch::Timer>::now)
}

/// 时钟频率（Hz）：把 [`kcore_now`] 的 tick 换算成时间需要它。
///
/// ABI 约定：`0` = 未知 / 不可换算（机器真相里是显式 `None`）——组件必须
/// 处理该情形，不得把 0 当真实频率做除法。
extern "C" fn kcore_timebase_hz() -> u64 {
    with_core_critical(|| {
        machine::committed().map_or(0, |m| m.timebase_frequency.map_or(0, |hz| hz.get()))
    })
}

extern "C" fn kcore_machine_cpu_count() -> u32 {
    with_core_critical(|| {
        machine::committed().map_or(0, |m| u32::try_from(m.cpu_info.len()).unwrap_or(0))
    })
}

extern "C" fn kcore_machine_has_hart(hart_id: u32) -> i32 {
    with_core_critical(|| {
        let Some(machine) = machine::committed() else {
            return 0;
        };
        machine
            .cpu_info
            .iter()
            .any(|cpu| cpu.hardware_id.raw() == u64::from(hart_id)) as i32
    })
}

// ---------------------------------------------------------------------------
// Category 4：System query（已提交 Core 真相的只读查询；counts → u32）
// ---------------------------------------------------------------------------

extern "C" fn kcore_free_page_count() -> u32 {
    with_core_critical(|| {
        memory::free_block_counts()
            .iter()
            .enumerate()
            .skip(memory::HEAP_MIN_ORDER)
            .map(|(order, &blocks)| blocks * (1usize << (order - memory::HEAP_MIN_ORDER)))
            .sum::<usize>() as u32
    })
}

extern "C" fn kcore_task_count() -> u32 {
    with_core_critical(|| task::get_task_table().lock().len() as u32)
}

extern "C" fn kcore_component_count() -> u32 {
    with_core_critical(|| registry::get_registry().lock().len() as u32)
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

/// 请求 Core 用**默认配置**创建组件实例（store → loader 放段/重定位 → registry →
/// `kcomp_instance_create` 全链，与 monitor `load` 同源）。
///
/// 这是 `kcore_component_create` 的便利入口（`config_abi = 0`，无 config 负载）。
/// 返回 ComponentId raw / `-Errno`
/// （`EINVAL` 名字非法；其余见 [`ComponentLoadError::abi_status`]——组件的 create
/// 返回码**原样**透传，不塌缩成 `EIO`）。
extern "C" fn kcore_component_load(name_ptr: *const u8, name_len: usize, domain: u32) -> i32 {
    with_core_critical(|| {
        let Some(name) = checked_name(name_ptr, name_len) else {
            return Errno::EINVAL.code();
        };
        use crate::generated::abi::ExecutionDomain as WireDomain;
        let kind = match domain {
            n if n == WireDomain::KernelNative as u32 => ExecutionDomain::KernelNative,
            n if n == WireDomain::IsolatedNative as u32 => ExecutionDomain::IsolatedNative,
            n if n == WireDomain::SandboxedNative as u32 => ExecutionDomain::SandboxedNative,
            _ => return Errno::EINVAL.code(),
        };
        match crate::component::load::load_and_start(name, kind) {
            Ok(id) => id.raw() as i32,
            Err(error) => error.abi_status(),
        }
    })
}

/// 用指定 config 负载创建一个新实例（`docs/architecture/component-lifecycle.md` §4）。
///
/// 每次 instantiate 都从 artifact 重新放段 / 重定位（新实例、新 id、独立
/// writable image state）。`args` 是组件自定义的 C 布局小结构，Core 视为
/// **不透明字节**（只在调用期间借用，不持久化、不解释）。
///
/// 成功 = 0，实例 id（`u32`）写入 `*out_instance`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` `args` / `out_instance` 为空 / `EINVAL` 名字非法 /
/// 其余见 [`ComponentLoadError::abi_status`]——组件的 create 返回码**原样**透传，
/// 不塌缩成 `EIO`）。
extern "C" fn kcore_component_create(
    image_name: *const u8,
    image_name_len: usize,
    args: *const KcompCreateArgs,
    out_instance: *mut u32,
) -> i32 {
    with_core_critical(|| {
        if args.is_null() || out_instance.is_null() {
            return Errno::EFAULT.code();
        }
        let Some(name) = checked_name(image_name, image_name_len) else {
            return Errno::EINVAL.code();
        };
        // SAFETY: 调用方保证 args 指向调用期间有效的 KcompCreateArgs（C ABI 契约）；
        // Core 只在本次调用内借用它。
        let args = unsafe { &*args };
        // create 带组件配置；未携带部署域，默认 KernelNative。
        match crate::component::load::create_component(name, args, ExecutionDomain::KernelNative) {
            Ok(id) => {
                // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
                unsafe { core::ptr::write_unaligned(out_instance, id.raw()) };
                0
            }
            Err(error) => error.abi_status(),
        }
    })
}

// ---------------------------------------------------------------------------
// Category 9：Component endpoints（Contract / Endpoint）
// ---------------------------------------------------------------------------

/// 发布 endpoint（**staged**：`kcomp_instance_create` 期间只记录 pending，不创建
/// endpoint）。provider = 当前正在创建的实例（Core 记录，**不信任组件自报身份**）。
///
/// `contract` 是组合策略提供的契约身份（不透明 `u64`）；`kind` / `abi` 由**首次
/// 发布**建立契约真相，后续发布不一致在 commit 时拒绝；`port` 是 provider 定义的
/// 不透明 dispatch token（Core 从不解释）。端口名只要求在 provider 实例内唯一。
///
/// `api` / `ctx` 是 provider 交付的 **Direct** transport：`api` 指向 provider 的
/// `#[repr(C)]` function table、`ctx` 是 provider opaque state。Core **只存、
/// 永不解引用**，只在 [`kcore_endpoint_bind`] 选定 Direct 时原样交付。`port` +
/// image 的 `kcomp_service_dispatch` 服务 **Gate** transport；机制由 Core 在 bind
/// 时按两端执行域选定，组件不得自行选择。
///
/// create 返回 0 后 Core 原子提交该实例的 pending endpoints，因此本函数返回 `0`
/// 只表示"已记录 pending"——**不返回 EndpointId**（id 只在 commit 成功后存在，
/// 由 [`kcore_endpoint_lookup`] 发现）。
/// provider 由最内层活动 create 边界解析（嵌套创建 = 被创建的实例）；`Failed`
/// provider → `-EPERM`。返回 0 / `-Errno`（`EINVAL` 名字/kind 非法；`EPERM` 不在
/// create 上下文或 provider 已 `Failed`；其余见 `Errno::from(EndpointError)`）。
#[allow(clippy::too_many_arguments)]
extern "C" fn kcore_endpoint_publish(
    port_name: *const u8,
    port_name_len: usize,
    contract: u64,
    kind: u32,
    abi: u64,
    port: u32,
    api: *const (),
    ctx: *mut (),
) -> i32 {
    with_core_critical(|| {
        let Some(port_name) = checked_name(port_name, port_name_len) else {
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
        let mut endpoints = endpoint::get_endpoints().lock();
        match endpoints.stage_publish(
            &reg,
            provider,
            port_name,
            ContractId::from_raw(contract),
            kind,
            InterfaceAbi::from_raw(abi),
            port,
            api,
            ctx,
        ) {
            Ok(()) => 0,
            Err(error) => Errno::from(error).code(),
        }
    })
}

/// 组合期发现：`(provider, port_name, contract) → EndpointId`。
///
/// Core 校验的只有 **contract + 存活**（[`EndpointRegistry::discover`]：
/// endpoint `Live`、owner 存在且 `Ready`），**不校验 abi**——本 ABI 不携带 abi，
/// 交付的 EndpointId 是 opaque capability；consumer 用
/// [`kcore_endpoint_validate`] 核对契约 ABI（SDK `Endpoint<C>::from_id`）。
/// `provider` 是 consumer 显式给出的实例身份——身份不是权限，Core 只按真相解析。
///
/// 成功 = 0，EndpointId（`u64`）写入 `*out_endpoint`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EINVAL` 名字非法或契约不符 /
/// `ENOENT` 未发布或 endpoint 已死 / `ENODEV` provider 已不存在）。
extern "C" fn kcore_endpoint_lookup(
    provider: u32,
    port_name: *const u8,
    port_name_len: usize,
    contract: u64,
    out_endpoint: *mut u64,
) -> i32 {
    with_core_critical(|| {
        if out_endpoint.is_null() {
            return Errno::EFAULT.code();
        }
        let Some(port_name) = checked_name(port_name, port_name_len) else {
            return Errno::EINVAL.code();
        };
        let reg = registry::get_registry().lock();
        let endpoints = endpoint::get_endpoints().lock();
        match endpoints.discover(
            &reg,
            ComponentId::from_raw(provider),
            port_name,
            ContractId::from_raw(contract),
        ) {
            Ok(id) => {
                // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
                unsafe { core::ptr::write_unaligned(out_endpoint, id.raw()) };
                0
            }
            Err(error) => Errno::from(error).code(),
        }
    })
}

/// 只读校验一个已持有的 EndpointId：**contract + abi exact-match** +
/// 存活（[`EndpointRegistry::lookup`]：endpoint `Live`、owner 存在且 `Ready`）。
///
/// 无副作用、不分配、不改变任何状态。这是 consumer 在取得 id 后核对契约身份的
/// 窄入口（SDK `Endpoint<C>::from_id` / `lookup` 用它，把 id 变成 typed
/// `Endpoint<C>`）；**不**替代 [`kcore_endpoint_call`] 的逐次存活解析——调用路径
/// 仍独立重新校验存活。
///
/// 成功 = `0`；失败 = `-Errno`（`EINVAL` contract 或 abi 不符 /
/// `ENOENT` endpoint 未发布或已死 / `ENODEV` provider 已不存在）。
extern "C" fn kcore_endpoint_validate(endpoint: u64, contract: u64, abi: u64) -> i32 {
    with_core_critical(|| {
        let reg = registry::get_registry().lock();
        let endpoints = endpoint::get_endpoints().lock();
        match endpoints.lookup(
            &reg,
            EndpointId::from_raw(endpoint),
            ContractId::from_raw(contract),
            InterfaceAbi::from_raw(abi),
        ) {
            Ok(_) => 0,
            Err(error) => Errno::from(error).code(),
        }
    })
}

/// **bind：Core 在绑定时刻选定调用机制**（一次，运行期不再按调用重决策）。
///
/// 校验（与 [`kcore_endpoint_validate`] 同一入口：exact contract + abi + 存活）后，
/// 按 `(caller 执行域, provider 执行域)` 选定机制：
///
/// - `KCORE_ENDPOINT_MECHANISM_DIRECT`：`*out_api` / `*out_ctx` 写入 provider 发布
///   时交付的 function table 与 opaque state（Core 原样传递、不解引用）；
/// - `KCORE_ENDPOINT_MECHANISM_GATE`：`*out_api` / `*out_ctx` **不写**（保持调用方
///   原值），调用方改用 [`kcore_endpoint_call`]（同一 `endpoint` 即 call-gate handle）。
///
/// caller 必须处在某个组件执行边界内（否则 `-EPERM`）——机制选择需要 caller 的
/// 执行域；caller 已 `Failed` 同样 `-EPERM`（获取绑定 = 获取新能力）。
/// 不支持的组合（跨特权 / 同 AS 无法证明且 syscall-IPC 未实现）→ `-ENOTSUP`，
/// **绝不静默降级成 Direct**；Direct 选中但 provider 未交付 function table →
/// `-ENOTSUP`。
///
/// 成功 = `0`（机制 + 对应 transport 写入 out）；失败 = `-Errno`
/// （`EFAULT` 任一 out 为空 / `EINVAL` contract 或 abi 不符 /
/// `ENOENT` endpoint 未发布或已死 / `ENODEV` provider 已不存在 /
/// `EPERM` 无 caller 边界或 caller 已 `Failed` / `ENOTSUP` 无已实现机制）。
#[allow(clippy::too_many_arguments)]
extern "C" fn kcore_endpoint_bind(
    endpoint: u64,
    contract: u64,
    abi: u64,
    out_mechanism: *mut u32,
    out_api: *mut usize,
    out_ctx: *mut usize,
) -> i32 {
    with_core_critical(|| {
        if out_mechanism.is_null() || out_api.is_null() || out_ctx.is_null() {
            return Errno::EFAULT.code();
        }
        // caller 身份解析：机制选择需要 caller 的执行域，因此**必须**在组件执行
        // 边界内（monitor / 纯 Core 上下文没有可解析的 caller）。
        let Some(caller) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        if let Some(denied) = deny_if_failed(caller.component) {
            return denied;
        }
        let reg = registry::get_registry().lock();
        let endpoints = endpoint::get_endpoints().lock();
        match endpoints.bind(
            &reg,
            EndpointId::from_raw(endpoint),
            ContractId::from_raw(contract),
            InterfaceAbi::from_raw(abi),
            endpoint::instance_domain(&reg, caller.component),
        ) {
            Ok(bound) => {
                // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
                unsafe {
                    match bound.mechanism {
                        endpoint::Mechanism::Direct => {
                            core::ptr::write_unaligned(
                                out_mechanism,
                                crate::generated::abi::KCORE_ENDPOINT_MECHANISM_DIRECT,
                            );
                            core::ptr::write_unaligned(out_api, bound.record.api as usize);
                            core::ptr::write_unaligned(out_ctx, bound.record.ctx as usize);
                        }
                        endpoint::Mechanism::Gate => {
                            core::ptr::write_unaligned(
                                out_mechanism,
                                crate::generated::abi::KCORE_ENDPOINT_MECHANISM_GATE,
                            );
                            // Gate 不写 api/ctx：binding 不携带裸 function table。
                        }
                    }
                }
                0
            }
            Err(error) => Errno::from(error).code(),
        }
    })
}

/// 调用一个 endpoint：Core 控制的 **service-call 执行边界**（per-call Core
/// 拥有的 service stack + provider principal + panic containment；见
/// `component/call.rs` 与 `containment::call_component_service`）。
///
/// **传输状态 ≠ 方法状态**：返回值是本函数的**传输状态**（`0` / `-Errno`）；
/// provider 自己的 `i32` 返回写入 `*out_status`，**只在传输返回 `0` 时有意义**。
/// provider 返回的负 errno 绝不与 Core 生成的失败混淆。
///
/// `endpoint` 是组合期经 [`kcore_endpoint_lookup`] 交付的 opaque `EndpointId`
/// （发现路径只校验 contract + 存活；abi 由 [`kcore_endpoint_validate`] 核对；
/// 这里只重新做**存活解析**）。
/// `args` / `input` / `output` 只在本次调用期间借用：Core 只做结构校验
/// （长度非零时指针不得为空），**不解析其中的字节**。
///
/// 成功 = `0`（provider status 在 `*out_status`）；失败 = `-Errno`
/// （`EFAULT` `*out_status` 为空或 frame 结构非法；`EPERM` 无法解析 caller 或
/// caller 已 `Failed`；`ENOTSUP` caller 不在 KernelNative 域（Isolated 出站调用
/// 未实现）；`ENOENT` endpoint 未发布或已死；`ENODEV` owner /
/// image 已不存在；`EBUSY` provider 不在 `Ready`、inflight 溢出或**重入**（provider
/// 已在当前同步链上）；`EINVAL` 调用链上有 **IRQ 作用域**；`ENOMEM` Core 无法
/// 分配 service stack；`EIO` provider 在边界内 **panic**（已被标记 `Failed` 且
/// 其 endpoint 永久失效，caller 存活）；`ENOSYS` image 没有
/// `kcomp_service_dispatch`）。
#[allow(clippy::too_many_arguments)]
extern "C" fn kcore_endpoint_call(
    endpoint: u64,
    method: u32,
    args: *const u8,
    args_len: usize,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: usize,
    out_status: *mut i32,
) -> i32 {
    with_core_critical(|| {
        // out 指针校验在 ABI 边界（与其它导出同序；`call` 内部另有同一检查，
        // 服务直接调用者）。
        if out_status.is_null() {
            return Errno::EFAULT.code();
        }
        // 注意：provider 的 dispatcher 在 `call::endpoint_call` 内部经
        // `call_component_service` 的 service 边界运行——边界会把 Core ABI 深度
        // 挂起到 0，所以 provider 的 panic 仍然可逃逸；只有本函数自己的 Core
        // 代码（解析 / 记账 / 收尾）是非逃逸区。
        match call::endpoint_call(
            EndpointId::from_raw(endpoint),
            method,
            args,
            args_len,
            input,
            input_len,
            output,
            output_len,
            out_status,
        ) {
            Ok(()) => 0,
            Err(error) => Errno::from(error).code(),
        }
    })
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

/// Isolated / Sandbox 调用者能力门禁：非 KernelNative 域**没有已实现**的
/// MMIO / DMA / IRQ / 任务 / Core 资源路径（窄支持包络；私有 AS
/// 只映射共享 Core + 自身内存）→ `-ENOTSUP`，绝不静默按 KernelNative 语义执行。
///
/// 只作用于**获取 authority / 创建 work** 的入口（claim / irq register /
/// dma alloc / dma map / task create）；teardown（release / free / unmap）仍由
/// 各自的 owner 校验约束，不受本门禁限制。
fn deny_if_isolated(component: crate::component::ComponentId) -> Option<i32> {
    let reg = registry::get_registry().lock();
    (endpoint::instance_domain(&reg, component) != ExecutionDomain::KernelNative)
        .then_some(Errno::ENOTSUP.code())
}

/// 创建任务。requester = 当前 caller；`entry` 必须落在该实例的
/// 装载镜像内（越界指针一律拒绝）；`arg` 是 opaque 参数，Core 原样透传给任务
/// 入口（`typedef void (*)(void *)`），**任务归属仍来自 Core 的执行边界**，
/// 与 `arg` 内容无关。
///
/// 成功 = 0，TaskId（`u32`）写入 `*out_task`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller /
/// `ENOTSUP` caller 不在 KernelNative 域（Isolated 任务未实现）；
/// 其余见 `Errno::from(TaskError)`）。
extern "C" fn kcore_task_create(entry: usize, arg: *mut (), out_task: *mut u32) -> i32 {
    with_core_critical(|| {
        if out_task.is_null() {
            return Errno::EFAULT.code();
        }
        let Some(requester) = current_task_requester() else {
            return Errno::EPERM.code();
        };
        if let Some(denied) = deny_if_failed(requester) {
            return denied;
        }
        if let Some(denied) = deny_if_isolated(requester) {
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
    })
}

/// 启动任务：Core 验证当前 caller 是任务 owner 后才推进 Created → Runnable。
/// 返回 0 / `-Errno`。
extern "C" fn kcore_task_start(id: u32) -> i32 {
    with_core_critical(|| {
        let Some(requester) = current_task_requester() else {
            return Errno::EPERM.code();
        };
        status(task::start_task(requester, TaskId::from_raw(id)))
    })
}

/// Owner 提议初始 CPU；Core 校验、固定归属并通知 Online 目标。
extern "C" fn kcore_task_start_on(id: u32, cpu: u32) -> i32 {
    with_core_critical(|| {
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        status(task::start_task_on(
            ctx.component,
            TaskId::from_raw(id),
            crate::machine::CpuId::from_raw(cpu as usize),
        ))
    })
}

extern "C" fn kcore_cpu_current() -> u32 {
    with_core_critical(|| crate::smp::current_cpu().raw() as u32)
}

/// 让出 CPU：Running → Runnable + 调度切换。任务再次被选中时返回 0。
/// 返回 0 / `-Errno`。
///
/// 注意：切换离开期间 Core ABI 深度由调度帧挂起/恢复（`sched::schedule_next`），
/// 本包装的 +1/-1 在任务被重新调度、`yield_current` 返回后仍然平衡。
extern "C" fn kcore_task_yield() -> i32 {
    with_core_critical(|| status(sched::yield_current()))
}

/// 阻塞当前任务，等它被 unpark 并再次调度后从此调用返回。
extern "C" fn kcore_task_park() -> i32 {
    with_core_critical(|| {
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        if let Some(denied) = deny_if_failed(ctx.component) {
            return denied;
        }
        if let Some(denied) = deny_if_isolated(ctx.component) {
            return denied;
        }
        status(sched::park_current())
    })
}

/// 让同 owner 的任务变为 Runnable，或为它保留一次提前到达的 unpark permit。
extern "C" fn kcore_task_unpark(id: u32) -> i32 {
    with_core_critical(|| {
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        if let Some(denied) = deny_if_failed(ctx.component) {
            return denied;
        }
        if let Some(denied) = deny_if_isolated(ctx.component) {
            return denied;
        }
        status(sched::unpark_task(ctx.component, TaskId::from_raw(id)))
    })
}

/// 退出：Running → Exited + 调度切换。**控制权永不回到本任务**——若还有
/// Runnable 任务则它们接管；全部退出后回到调度器锚点（调 `kcore_sched_run`
/// 的上下文）。返回 0 / `-Errno`。
extern "C" fn kcore_task_exit() -> i32 {
    with_core_critical(|| status(sched::exit_current()))
}

/// 只读查询任务状态（Core 真相的编码视图）：
/// 0=Created 1=Runnable 2=Running 3=Blocked 4=Exited；`-ESRCH` = 不存在。
extern "C" fn kcore_task_state(id: u32) -> i32 {
    with_core_critical(|| {
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
    })
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
///
/// 任务切换期间 Core ABI 深度由调度帧挂起/恢复（`sched::schedule_next`）：
/// 任务代码深度 0（可逃逸），锚点恢复本包装的深度（切换期间不可逃逸）。
extern "C" fn kcore_sched_run() -> i32 {
    with_core_critical(|| status(sched::run()))
}

/// 选择调度策略：把 `endpoint`（`scheduler.policy` 契约）提交为 Core 的调度配置。
///
/// 只记 `EndpointId`（+ 为策略执行准备的 Core 栈）——**不发布任何东西**；组合方
/// （core_test / kbench / monitor）在 provider 的 create 返回 0 之后显式发现 +
/// 选择，Core 的调度路径不按名字发现。校验 contract + abi + 存活 + provider 有
/// `kcomp_service_dispatch`；IRQ / service-call / policy 执行内拒绝（`-EINVAL`）。
/// 返回 0 / `-Errno`（见 [`SchedError`] 的映射）。
extern "C" fn kcore_sched_set_policy(endpoint: u64) -> i32 {
    with_core_critical(|| status(sched::set_policy(EndpointId::from_raw(endpoint))))
}

// ---------------------------------------------------------------------------
// Category 8：Device / IRQ / DMA（mechanism-first；ownership + 真实机制，无 per-access 鉴权）
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
    with_core_critical(|| {
        if out_device_id.is_null() {
            return Errno::EFAULT.code();
        }
        let compatible = if compatible_len == 0 && !compatible_ptr.is_null() {
            &[][..] // 无过滤发现；仍然只返回 DeviceId，不交付任何权限。
        } else {
            let Some(compatible) = checked_name(compatible_ptr, compatible_len) else {
                return Errno::EINVAL.code();
            };
            compatible
        };
        match machine::nth_compatible(compatible, ordinal) {
            Ok(device) => {
                // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
                unsafe { core::ptr::write_unaligned(out_device_id, device.raw()) };
                0
            }
            Err(error) => Errno::from(error).code(),
        }
    })
}

/// 认领**一台确切设备**：Core 记 owner 并返回本执行域下的**主窗口** MMIO。
///
/// **认领是整台设备的所有权**：返回的窗口是 `DeviceDescriptor.spaces[0]`
/// （`spaces` 按固件顺序，第一个是主窗口）。其余窗口由 boot/Core 保留与映射，
/// 本阶段没有组件侧索引窗口的 API。
///
/// **访问强制不在 Core 数据路径上**：KernelNative 与 Core 同特权，`*out_mmio`
/// 直接是寄存器基址——driver 之后自己 volatile 读写，steady state 不再进 Core。
/// 真正的访问强制来自执行域：Isolated 在 claim 时把窗口映射进组件地址空间，
/// `*out_mmio` 返回 mapped VA，未映射访问由页表 fault（该路径未实现）。
///
/// 成功 = 0，`*out_mmio`（指针宽）与 `*out_len` 写入（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller 或 caller 已
/// `Failed` / `ENOTSUP` caller 不在 KernelNative 域（Isolated 无 MMIO 窗口）
/// 或设备主窗口不是 MMIO（PIO / 无窗口）/ `ENODEV` 设备不存在 / `EBUSY`
/// 设备已被认领或已 quarantine）。
extern "C" fn kcore_device_claim(
    device_id: u32,
    out_mmio: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    with_core_critical(|| {
        if out_mmio.is_null() || out_len.is_null() {
            return Errno::EFAULT.code();
        }
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        if let Some(denied) = deny_if_failed(ctx.component) {
            return denied;
        }
        if let Some(denied) = deny_if_isolated(ctx.component) {
            return denied;
        }
        match device::claim(&ctx, machine::DeviceId::from_raw(device_id)) {
            Ok(mapping) => {
                // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
                unsafe {
                    core::ptr::write_unaligned(out_mmio, mapping.mmio);
                    core::ptr::write_unaligned(out_len, mapping.mmio_len);
                }
                0
            }
            Err(error) => Errno::from(error).code(),
        }
    })
}

/// 释放设备 ownership。
///
/// **拆机顺序**：仍有 live IRQ route / DMA mapping 时返回 `-EBUSY`——先静默设备、
/// 释放 IRQ/DMA，再释放 device。返回 0 / `-Errno`（`EPERM` 无法解析 caller /
/// `ENODEV` 设备不存在或未认领 / `EACCES` 非 owner / `EBUSY` 仍有子项）。
extern "C" fn kcore_device_release(device_id: u32) -> i32 {
    with_core_critical(|| {
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        status(device::release(
            &ctx,
            machine::DeviceId::from_raw(device_id),
        ))
    })
}

// ---------------------------------------------------------------------------
// Category 8（续）：IRQ routes（device-anchored；native callback only）
// ---------------------------------------------------------------------------

/// 注册该设备某条中断资源的投递目标（组件处理函数 + opaque context）。
///
/// `resource_index` 是该设备 `DeviceDescriptor.interrupts` 中的下标（二维锚点
/// `(DeviceId, resource_index)`）；`handler` 是组件提供的
/// `extern "C" fn(ctx: *mut ())`，`ctx` 原样回传，Core 不解引用。route 随
/// device release / 组件失败一起消失——之后不会再有回调进它的代码。
///
/// 成功 = 0；失败 = `-Errno`（`EPERM` 无法解析 caller 或已 Failed /
/// `ENOTSUP` caller 不在 KernelNative 域（Isolated 无 IRQ 路径）/
/// `ENODEV` 设备不存在、资源越界或无（未绑定）中断线 /
/// `EACCES` caller 不是设备 owner / `EBUSY` 该逻辑线已挂在别的资源 key 下）。
extern "C" fn kcore_irq_register(
    device_id: u32,
    resource_index: u32,
    handler: irq::IrqHandler,
    ctx: *mut (),
) -> i32 {
    with_core_critical(|| {
        let Some(caller) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        if let Some(denied) = deny_if_failed(caller.component) {
            return denied;
        }
        if let Some(denied) = deny_if_isolated(caller.component) {
            return denied;
        }
        status(irq::register(
            &caller,
            machine::DeviceId::from_raw(device_id),
            resource_index,
            handler,
            ctx,
        ))
    })
}

/// 使能该设备某条中断资源：Core 验证 route 后配置中断控制器。
/// 成功 = 0；失败 = `-Errno`（`EPERM` 无法解析 caller / `ENODEV` 无中断线 /
/// `EACCES` 非 owner / `EINVAL` 尚未注册 handler）。
extern "C" fn kcore_irq_enable(device_id: u32, resource_index: u32) -> i32 {
    with_core_critical(|| {
        let Some(caller) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        status(irq::enable(
            &caller,
            machine::DeviceId::from_raw(device_id),
            resource_index,
        ))
    })
}

/// 关断该设备某条中断资源（控制器层）。返回 0 / `-Errno`。
extern "C" fn kcore_irq_disable(device_id: u32, resource_index: u32) -> i32 {
    with_core_critical(|| {
        let Some(caller) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        status(irq::disable(
            &caller,
            machine::DeviceId::from_raw(device_id),
            resource_index,
        ))
    })
}

/// 释放该设备的 IRQ route：撤销 route（此后不再投递给已死 owner）并关断控制器线。
/// 返回 0 / `-Errno`。
extern "C" fn kcore_irq_release(device_id: u32, resource_index: u32) -> i32 {
    with_core_critical(|| {
        let Some(caller) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        status(irq::release(
            &caller,
            machine::DeviceId::from_raw(device_id),
            resource_index,
        ))
    })
}

// ---------------------------------------------------------------------------
// Category 8（续）：DMA（allocation 与 mapping 分离）
// ---------------------------------------------------------------------------

/// 分配一段物理连续的 DMA 缓冲（CPU-visible）。**device-agnostic**：分配后端不
/// 知道 VirtIO / NVMe / 具体 DeviceId，只负责给一块满足约束的内存。
///
/// 成功 = 0，`*out_ptr`（指针宽）与 `*out_len` 写入（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EPERM` 无法解析 caller 或已 Failed /
/// `ENOTSUP` caller 不在 KernelNative 域（Isolated 无 DMA 路径）/
/// `EINVAL` 尺寸非法 / `ENOMEM` 物理内存耗尽）。
extern "C" fn kcore_dma_alloc(size: usize, out_ptr: *mut *mut u8, out_len: *mut usize) -> i32 {
    with_core_critical(|| {
        if out_ptr.is_null() || out_len.is_null() {
            return Errno::EFAULT.code();
        }
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        if let Some(denied) = deny_if_failed(ctx.component) {
            return denied;
        }
        if let Some(denied) = deny_if_isolated(ctx.component) {
            return denied;
        }
        match dma::alloc(ctx.component, size) {
            Ok(buffer) => {
                // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
                unsafe {
                    core::ptr::write_unaligned(out_ptr, buffer.ptr);
                    core::ptr::write_unaligned(out_len, buffer.len);
                }
                0
            }
            Err(error) => Errno::from(error).code(),
        }
    })
}

/// 释放一段 DMA 缓冲。backing lease 进 Core 私有 QUARANTINE（**不 free**，设备
/// 可能仍在 DMA——DMA lifecycle safety，见 `resource::dma`）。返回 0 / `-Errno`。
extern "C" fn kcore_dma_free(ptr: *mut u8) -> i32 {
    with_core_critical(|| {
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        status(dma::free(ctx.component, ptr))
    })
}

/// 把一个 buffer 映射给某台设备，返回**设备可见地址** + mapping identity。
///
/// No-IOMMU：device address == buffer 地址（identity）；IOMMU 只需在 Core
/// 内部把 buffer PA → IOVA（或经 bounce buffer），driver 不变。只有设备 owner 能
/// 把自己的 buffer 映射给该设备。
///
/// 成功 = 0，`*out_device_addr`（`u64`）与 `*out_mapping`（`u64`）写入；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EINVAL` direction 非法或范围非法 /
/// `EPERM` 无法解析 caller 或已 Failed / `ENOTSUP` caller 不在 KernelNative 域 /
/// `ENODEV` 设备不存在 / `EACCES` 非 owner）。
extern "C" fn kcore_dma_map(
    device_id: u32,
    ptr: *mut u8,
    len: usize,
    direction: i32,
    out_device_addr: *mut u64,
    out_mapping: *mut u64,
) -> i32 {
    with_core_critical(|| {
        if out_device_addr.is_null() || out_mapping.is_null() {
            return Errno::EFAULT.code();
        }
        let Some(direction) = dma::DmaDirection::from_i32(direction) else {
            return Errno::EINVAL.code();
        };
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        if let Some(denied) = deny_if_failed(ctx.component) {
            return denied;
        }
        if let Some(denied) = deny_if_isolated(ctx.component) {
            return denied;
        }
        match dma::map(
            &ctx,
            machine::DeviceId::from_raw(device_id),
            ptr,
            len,
            direction,
        ) {
            Ok(mapping) => {
                // SAFETY: out 指针可写性由调用方保证（C ABI 契约）；unaligned 写防未对齐 UB。
                unsafe {
                    core::ptr::write_unaligned(out_device_addr, mapping.device_addr as u64);
                    core::ptr::write_unaligned(out_mapping, mapping.id);
                }
                0
            }
            Err(error) => Errno::from(error).code(),
        }
    })
}

/// 撤销一条 DMA mapping。返回 0 / `-Errno`（`ENOENT` mapping 不存在）。
///
/// 不解析 ambient caller：mapping 归属 Core truth（device owner），而 unmap 可能
/// 发生在 consumer 的任务上下文（provider 方法被直接调用）。
extern "C" fn kcore_dma_unmap(mapping: u64) -> i32 {
    with_core_critical(|| status(dma::unmap(mapping)))
}

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

    /// Direct function table 的替身地址（Core 只存、不解引用）。
    static TABLE: [u8; 8] = [0; 8];

    #[test]
    fn resolves_all_entries() {
        use alloc::string::String;
        for name in [
            &b"kcore_trace_read"[..],
            &b"kcore_trace_stats"[..],
            &b"kcore_memory_acquire"[..],
            &b"kcore_memory_release"[..],
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
            &b"kcore_task_create"[..],
            &b"kcore_task_start"[..],
            &b"kcore_task_yield"[..],
            &b"kcore_task_park"[..],
            &b"kcore_task_unpark"[..],
            &b"kcore_task_exit"[..],
            &b"kcore_task_state"[..],
            &b"kcore_panic_escape"[..],
            &b"kcore_sched_run"[..],
            &b"kcore_sched_set_policy"[..],
            &b"kcore_device_nth"[..],
            &b"kcore_device_claim"[..],
            &b"kcore_device_release"[..],
            &b"kcore_irq_register"[..],
            &b"kcore_irq_enable"[..],
            &b"kcore_irq_disable"[..],
            &b"kcore_irq_release"[..],
            &b"kcore_dma_alloc"[..],
            &b"kcore_dma_free"[..],
            &b"kcore_dma_map"[..],
            &b"kcore_dma_unmap"[..],
            &b"kcore_endpoint_publish"[..],
            &b"kcore_endpoint_lookup"[..],
            &b"kcore_endpoint_validate"[..],
            &b"kcore_endpoint_bind"[..],
            &b"kcore_endpoint_call"[..],
        ] {
            assert!(resolve(name).is_some(), "{}", String::from_utf8_lossy(name));
        }
    }

    /// 机械一致性检查：**除 `kcore_panic_escape` 外每个导出体都包在
    /// `with_core_critical` 里**（Core panic 必须致命，见模块文档）。
    ///
    /// 为什么是文本检查：函数体是否被包装无法从 ABI 观察——调用每个导出都会
    /// 产生副作用/依赖真实 Core 状态（host 不可行）。实现部分（test module 之前）
    /// 里非注释行的 `with_core_critical(` 调用点数量必须等于
    /// `EXPORTS.len() - 1`。新增导出会同时改变两边，漏包即失败。
    #[test]
    fn every_ordinary_export_body_is_core_critical() {
        let implementation_half = include_str!("export.rs")
            .split("\n#[cfg(test)]")
            .next()
            .expect("export.rs always carries its test module");
        let query_half = include_str!("export/query.rs")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap();
        let user_half = include_str!("export/user.rs");
        let wrapped = implementation_half
            .lines()
            .chain(query_half.lines())
            .chain(user_half.lines())
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.contains("with_core_critical("))
            .count();
        assert_eq!(
            wrapped,
            EXPORTS.len() - 1,
            "every export except the escape request must wrap its body"
        );
    }

    #[test]
    fn observation_outputs_and_deployment_reject_invalid_inputs() {
        assert_eq!(
            kcore_component_nth(0, core::ptr::null_mut(), core::ptr::null_mut(), 0),
            Errno::EFAULT.code()
        );
        assert_eq!(
            kcore_endpoint_nth(0, core::ptr::null_mut(), core::ptr::null_mut(), 0),
            Errno::EFAULT.code()
        );
        assert_eq!(
            kcore_component_load(b"x".as_ptr(), 1, u32::MAX),
            Errno::EINVAL.code()
        );
    }

    #[test]
    fn rejects_unknown_names() {
        assert_eq!(resolve(b"kcore_frame_alloc"), None);
        assert_eq!(resolve(b"kcore_alloc_region"), None);
        assert_eq!(resolve(b"kcore_address_space_map"), None);
        assert_eq!(resolve(b"kcore_task_table_create"), None);
        assert_eq!(resolve(b"kcore_registry_declare"), None);
        assert_eq!(resolve(b"kcore_context_switch"), None);
        assert_eq!(resolve(b"kcore_mmio_grant"), None, "MMIO 中间层已删除");
        assert_eq!(
            resolve(b"kcore_mmio_claim"),
            None,
            "设备认领只能走 device_claim"
        );
        assert_eq!(resolve(b"kcore_irq_claim"), None, "IRQ 锚在 DeviceId");
        assert_eq!(resolve(b"kcore_irq_poll"), None, "polled 模型已 defer");
        assert_eq!(resolve(b"kcore_"), None);
        assert_eq!(resolve(b""), None);
    }

    #[test]
    fn names_are_exact_not_prefix() {
        assert!(resolve(b"kcore_log_line2").is_none(), "禁止前缀匹配");
        assert!(resolve(b"x?kcore_log_line").is_none(), "禁止后缀匹配");
    }

    /// DMA 方向 ABI 编码锚定（`docs/architecture/driver-model.md`）：`as_i32` 与
    /// `DmaDirection::from_i32` 必须互为逆，且值固定为 0/1/2；`kcomp-sdk` 的镜像
    /// 枚举同值（SDK 侧有对应测试 `dma_direction_encoding_is_stable`）。
    #[test]
    fn dma_direction_encoding_is_stable() {
        use crate::resource::dma::DmaDirection;
        assert_eq!(DmaDirection::ToDevice.as_i32(), 0);
        assert_eq!(DmaDirection::FromDevice.as_i32(), 1);
        assert_eq!(DmaDirection::Bidirectional.as_i32(), 2);
        for direction in [
            DmaDirection::ToDevice,
            DmaDirection::FromDevice,
            DmaDirection::Bidirectional,
        ] {
            assert_eq!(DmaDirection::from_i32(direction.as_i32()), Some(direction));
        }
        assert_eq!(DmaDirection::from_i32(3), None, "越界编码必须被拒绝");
    }

    /// `kcore_panic_escape`：host 无活动 containment 边界时安全地返回 `-EPERM`
    /// （不触发 context switch）。活动边界下永不返回，只能由 QEMU/ArchTest 验证。
    #[test]
    fn panic_escape_without_boundary_returns_eperm() {
        // 与安装 test boundary 的测试互斥：否则 `active_escape()` 检查与调用之间
        // 可能被别的测试装上边界，escape 会切到那个边界上（host 并行测试竞态）。
        let _boundary = crate::component::containment::test_boundary_lock();
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

        // kcore_device_claim：任一 out 为 null → EFAULT（早于 caller 解析）。
        let claim = resolve(b"kcore_device_claim").unwrap();
        let claim: extern "C" fn(u32, *mut *mut u8, *mut usize) -> i32 =
            unsafe { core::mem::transmute(claim) };
        let (mut mmio_ptr, mut mmio_len) = (core::ptr::null_mut(), 0usize);
        assert_eq!(claim(0, core::ptr::null_mut(), &mut mmio_len), -14);
        assert_eq!(claim(0, &mut mmio_ptr, core::ptr::null_mut()), -14);

        // kcore_device_release：host 无 caller → EPERM（不是 panic）。
        let _release: extern "C" fn(u32) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_device_release").unwrap()) };

        // IRQ register/enable/disable/release（二维锚点：device + resource index）：
        // host 无 caller → EPERM（不是 panic）。
        let _irq_register: extern "C" fn(u32, u32, extern "C" fn(*mut ()), *mut ()) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_irq_register").unwrap()) };
        let _irq_enable: extern "C" fn(u32, u32) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_irq_enable").unwrap()) };
        let _irq_disable: extern "C" fn(u32, u32) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_irq_disable").unwrap()) };
        let _irq_release: extern "C" fn(u32, u32) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_irq_release").unwrap()) };

        // DMA alloc：out 为空 → EFAULT（早于 caller 解析）。
        let dma_alloc: extern "C" fn(usize, *mut *mut u8, *mut usize) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_dma_alloc").unwrap()) };
        let mut dma_ptr = core::ptr::null_mut();
        let mut dma_len = 0usize;
        assert_eq!(dma_alloc(4096, core::ptr::null_mut(), &mut dma_len), -14);
        assert_eq!(dma_alloc(4096, &mut dma_ptr, core::ptr::null_mut()), -14);

        // DMA map：out 为空 → EFAULT；direction 非法 → EINVAL（都早于 caller 解析）。
        let dma_map: extern "C" fn(u32, *mut u8, usize, i32, *mut u64, *mut u64) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_dma_map").unwrap()) };
        let (mut dev_addr, mut mapping) = (0u64, 0u64);
        assert_eq!(
            dma_map(
                0,
                core::ptr::null_mut(),
                16,
                0,
                core::ptr::null_mut(),
                &mut mapping
            ),
            -14
        );
        assert_eq!(
            dma_map(
                0,
                core::ptr::null_mut(),
                16,
                0,
                &mut dev_addr,
                core::ptr::null_mut()
            ),
            -14
        );
        assert_eq!(
            dma_map(0, core::ptr::null_mut(), 16, 9, &mut dev_addr, &mut mapping),
            -22
        );

        // DMA free / unmap：host 无 caller → EPERM（不是 panic）。
        let _dma_free: extern "C" fn(*mut u8) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_dma_free").unwrap()) };
        let _dma_unmap: extern "C" fn(u64) -> i32 =
            unsafe { core::mem::transmute(resolve(b"kcore_dma_unmap").unwrap()) };
    }

    /// Failed 实例门禁：获取资源 / 创建 work 的入口一律 `-EPERM`；teardown
    /// （按 DeviceId 锚定的 release）不受门禁限制。
    #[test]
    fn failed_component_is_denied_new_resources() {
        use crate::component::{containment, registry};

        extern "C" fn irq_stub(_ctx: *mut ()) {}

        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        crate::resource::init();

        // Given：一个被标记 Failed 的组件。
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    crate::component::endpoint::ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.mark_failed(id).unwrap();
            id
        };

        // When / Then：身份解析到该 Failed 组件（create 边界）；acquiring 入口
        // 全部 `-EPERM`。
        containment::with_test_init_boundary(Some(id), || {
            let (mut ptr, mut len, mut mapping) = (core::ptr::null_mut(), 0usize, 0u64);
            let mut out_task = 0u32;
            assert_eq!(
                kcore_device_claim(0, &mut ptr, &mut len),
                Errno::EPERM.code()
            );
            assert_eq!(
                kcore_irq_register(0, 0, irq_stub, core::ptr::null_mut()),
                Errno::EPERM.code()
            );
            assert_eq!(
                kcore_dma_alloc(4096, &mut ptr, &mut len),
                Errno::EPERM.code()
            );
            assert_eq!(
                kcore_dma_map(0, core::ptr::null_mut(), 16, 0, &mut 0u64, &mut mapping),
                Errno::EPERM.code()
            );
            assert_eq!(
                kcore_task_create(0x1000, core::ptr::null_mut(), &mut out_task),
                Errno::EPERM.code()
            );
            assert_eq!(
                kcore_endpoint_publish(
                    b"svc".as_ptr(),
                    3,
                    0xAB,
                    1,
                    0xCD,
                    7,
                    core::ptr::null(),
                    core::ptr::null_mut(),
                ),
                Errno::EPERM.code()
            );
        });
    }

    /// Isolated 实例门禁：非 KernelNative 域没有已实现的 MMIO / DMA / IRQ / 任务 /
    /// 出站调用路径 → acquiring 入口一律 `-ENOTSUP`（窄支持包络）。
    ///
    /// 对照：同一 `device_claim` 在 KernelNative 边界下**不**被域门禁拒绝，而是走到
    /// 设备解析（`-ENODEV`）——证明这是按域拒绝，不是"所有调用都拒绝"。
    #[test]
    fn isolated_component_is_denied_core_resource_exports() {
        use crate::component::{containment, registry};

        extern "C" fn irq_stub(_ctx: *mut ()) {}

        let _boundary = containment::test_boundary_lock();
        registry::init();
        crate::resource::init();

        // Given：一个 Isolated 实例（Starting = 活实例，但域没有已实现路径）。
        let isolated = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    ExecutionDomain::IsolatedNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };

        // When / Then：获取 authority / 创建 work 的入口全部 `-ENOTSUP`。
        containment::with_test_init_boundary(Some(isolated), || {
            let (mut ptr, mut len, mut mapping) = (core::ptr::null_mut(), 0usize, 0u64);
            let mut out_task = 0u32;
            assert_eq!(
                kcore_device_claim(0, &mut ptr, &mut len),
                Errno::ENOTSUP.code()
            );
            assert_eq!(
                kcore_irq_register(0, 0, irq_stub, core::ptr::null_mut()),
                Errno::ENOTSUP.code()
            );
            assert_eq!(
                kcore_dma_alloc(4096, &mut ptr, &mut len),
                Errno::ENOTSUP.code()
            );
            assert_eq!(
                kcore_dma_map(0, core::ptr::null_mut(), 16, 0, &mut 0u64, &mut mapping),
                Errno::ENOTSUP.code()
            );
            assert_eq!(
                kcore_task_create(0x1000, core::ptr::null_mut(), &mut out_task),
                Errno::ENOTSUP.code()
            );
            let mut out_status = 0i32;
            assert_eq!(
                kcore_endpoint_call(
                    999,
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null(),
                    0,
                    core::ptr::null_mut(),
                    0,
                    &mut out_status,
                ),
                Errno::ENOTSUP.code(),
                "Isolated 出站调用（跨 AS Gate 未实现）必须显式拒绝"
            );
        });

        // 对照：KernelNative 边界下 device_claim 不被域门禁拒绝——设备解析失败是
        // 它自己的错误（`-ENODEV`），不是 `-ENOTSUP`。
        let native = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };
        containment::with_test_init_boundary(Some(native), || {
            let (mut ptr, mut len) = (core::ptr::null_mut(), 0usize);
            assert_eq!(
                kcore_device_claim(9999, &mut ptr, &mut len),
                Errno::ENODEV.code(),
                "KernelNative 不受域门禁限制，失败来自设备解析"
            );
        });
    }

    /// `kcore_device_claim` 的窗口契约（host 直调导出）：认领是整台设备的
    /// ownership，返回**主窗口** `DeviceDescriptor.spaces[0]`；PIO 主窗口 / 无窗口
    /// → `-ENOTSUP`（后续窗口即使存在也不改变结论）。
    #[test]
    fn kcore_device_claim_returns_primary_window_and_rejects_non_mmio_primary() {
        use crate::component::{containment, registry};
        use crate::machine::{CpuInfo, DeviceDescriptor, HardwareCpuId, IoSpace, MemoryRegion};

        let _boundary = containment::test_boundary_lock();
        let _machine = crate::machine::test_support::GUARD.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        crate::resource::init();

        // [0] 多窗口 MMIO：[1] PIO 主窗口 + 第二 MMIO 窗口。
        let mut devices = alloc::vec![DeviceDescriptor::empty(); 2];
        devices[0].spaces = alloc::vec![
            IoSpace::Mmio {
                base: 0x1000_8000,
                size: 0x1000,
            },
            IoSpace::Mmio {
                base: 0x2000_0000,
                size: 0x2000,
            },
        ]
        .into_boxed_slice();
        devices[1].spaces = alloc::vec![
            IoSpace::Pio {
                base: 0x3f8,
                size: 8
            },
            IoSpace::Mmio {
                base: 0x3000_0000,
                size: 0x1000,
            },
        ]
        .into_boxed_slice();
        let info = crate::machine::test_support::snapshot(
            HardwareCpuId::from_raw(0),
            core::num::NonZeroU64::new(10_000_000),
            alloc::vec![CpuInfo {
                boot_cpu: true,
                hardware_id: HardwareCpuId::from_raw(0),
            }],
            alloc::vec![MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }],
            devices,
        );
        crate::machine::test_support::install(info);
        crate::resource::test_support::reinstall();

        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };

        containment::with_test_init_boundary(Some(id), || {
            let (mut ptr, mut len) = (core::ptr::null_mut(), 0usize);
            assert_eq!(kcore_device_claim(0, &mut ptr, &mut len), 0);
            assert_eq!(ptr as usize, 0x1000_8000, "claim 返回主窗口 spaces[0]");
            assert_eq!(len, 0x1000);
            assert_eq!(
                kcore_device_claim(1, &mut ptr, &mut len),
                Errno::ENOTSUP.code(),
                "PIO 主窗口（后面还有 MMIO）必须拒绝"
            );
        });
    }

    /// 锁定 `deny_if_failed` 的**已知范围**：只拒绝 `Failed`（逻辑死亡）实例；
    /// 已 `Stopped` 的实例**不在**其内。
    ///
    /// review `docs/notes/resource-model-review.md` §C.5 明确记录这是 known gap：
    /// 其它"不应获得新权威"的状态（`Stopping` / `Stopped`）未被该门禁覆盖。
    /// 这里只 documenting + 锁定现状，**不修改行为**。
    #[test]
    fn deny_if_failed_denies_failed_only_not_stopped() {
        use crate::component::registry;

        registry::init();

        let failed = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    crate::component::endpoint::ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.mark_failed(id).unwrap();
            id
        };
        let stopped = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    crate::component::endpoint::ExecutionDomain::KernelNative,
                )
                .unwrap();
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

    /// `kcore_memory_acquire/release`：域视图往返（走真实导出函数，不是复刻逻辑）。
    ///
    /// 锁定契约语义：首次交付零初始化、`kind = KCORE_MEMORY_VIEW_LOCAL_VA`、
    /// `view.len >= min_len`、release 归还 backing（同尺寸再 acquire 复用同一块）；
    /// `min_len == 0` / `min_align` 非 2 的幂 → `EINVAL`；null out → `EFAULT`；
    /// absurd view（零 / 未页对齐 / 非 2 的幂 / 未知 kind）→ `EINVAL`。
    /// **无账本**：release 只凭 view 自身。
    #[test]
    fn memory_acquire_release_roundtrip() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        let acquire = resolve(b"kcore_memory_acquire").unwrap();
        let acquire: extern "C" fn(u64, u64, *mut MemoryView) -> i32 =
            unsafe { core::mem::transmute(acquire) };
        let release = resolve(b"kcore_memory_release").unwrap();
        let release: extern "C" fn(*const MemoryView) -> i32 =
            unsafe { core::mem::transmute(release) };

        let empty = MemoryView {
            kind: 0,
            reserved: 0,
            base: 0,
            len: 0,
        };

        // null out → EFAULT；size/align 非法 → EINVAL（都早于分配）。
        let mut view = empty;
        assert_eq!(acquire(1, 8, core::ptr::null_mut()), Errno::EFAULT.code());
        assert_eq!(acquire(0, 8, &mut view), Errno::EINVAL.code());
        assert_eq!(acquire(1, 0, &mut view), Errno::EINVAL.code());
        assert_eq!(acquire(1, 3, &mut view), Errno::EINVAL.code());

        // 成功：kind 由 Core 选定、len 覆盖请求、首次交付零初始化。
        assert_eq!(acquire(1, 8, &mut view), 0);
        assert_eq!(view.kind, KCORE_MEMORY_VIEW_LOCAL_VA);
        assert!(view.len >= 1 && view.len.is_power_of_two());
        assert_eq!(view.base % crate::memory::ALLOC_GRANULE as u64, 0);
        // SAFETY: view.base 来自 alloc_region（块容量 = view.len），host 下是有效内存。
        let head = unsafe { core::slice::from_raw_parts(view.base as *const u8, 16) };
        assert!(head.iter().all(|&b| b == 0), "首次交付必须零初始化");

        // release 归还 backing：同尺寸再次 acquire 必须复用同一块。
        let first = view;
        assert_eq!(release(&view), 0);
        assert_eq!(acquire(1, 8, &mut view), 0);
        assert_eq!((view.base, view.len), (first.base, first.len));
        assert_eq!(release(&view), 0);

        // null → EFAULT；absurd view → EINVAL（零 / 未页对齐 / 非 2 的幂 / 未知 kind）。
        assert_eq!(release(core::ptr::null()), Errno::EFAULT.code());
        assert_eq!(release(&empty), Errno::EINVAL.code());
        assert_eq!(
            release(&MemoryView {
                kind: KCORE_MEMORY_VIEW_LOCAL_VA,
                reserved: 0,
                base: 0x1001,
                len: 4096,
            }),
            Errno::EINVAL.code()
        );
        assert_eq!(
            release(&MemoryView {
                kind: KCORE_MEMORY_VIEW_LOCAL_VA,
                reserved: 0,
                base: 0x1000,
                len: 5000,
            }),
            Errno::EINVAL.code()
        );
        assert_eq!(
            release(&MemoryView {
                kind: 0,
                reserved: 0,
                base: first.base,
                len: first.len,
            }),
            Errno::EINVAL.code()
        );
    }

    /// `kcore_heap_alloc/dealloc`：KernelNative 共享堆后端往返（走**真实导出函数**，
    /// 不是复刻逻辑）。
    ///
    /// 锁定契约语义：分配可写、返回指针对齐；`dealloc` 用**原始 layout** 归还后，
    /// 同 layout 再分配必须复用同一块（slab / buddy 路由由 Layout 驱动）；
    /// `size == 0` / 非法 align → null；null ptr / `size == 0` → `EINVAL`。
    #[test]
    fn heap_alloc_dealloc_roundtrip() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        let alloc = resolve(b"kcore_heap_alloc").unwrap();
        let alloc: extern "C" fn(usize, usize) -> *mut u8 = unsafe { core::mem::transmute(alloc) };
        let dealloc = resolve(b"kcore_heap_dealloc").unwrap();
        let dealloc: extern "C" fn(*mut u8, usize, usize) -> i32 =
            unsafe { core::mem::transmute(dealloc) };

        // 非法入口：size == 0 / align = 0 / 非 2 的幂 align → null（不 panic）。
        assert!(alloc(0, 8).is_null());
        assert!(alloc(16, 0).is_null());
        assert!(alloc(16, 3).is_null());

        // 小对象（slab 路径）：可写、按 align 对齐。
        let ptr = alloc(24, 8);
        assert!(!ptr.is_null(), "small alloc must succeed");
        assert_eq!(ptr as usize % 8, 0);
        // SAFETY: ptr 来自成功 alloc，覆盖 24 字节。
        unsafe { ptr.write_bytes(0xA5, 24) };

        // 原始 layout 归还后，同 layout 再分配复用同一块（Layout 驱动路由）。
        assert_eq!(dealloc(ptr, 24, 8), 0);
        let again = alloc(24, 8);
        assert_eq!(again, ptr, "同 layout 的 slab 分配应复用释放的块");
        assert_eq!(dealloc(again, 24, 8), 0);

        // 大对象（buddy 路径，> 最大 slab class = 页/4）：同样按原始 layout 往返。
        let big = alloc(8192, 16);
        assert!(!big.is_null(), "large alloc must succeed");
        assert_eq!(big as usize % 16, 0);
        // SAFETY: big 来自成功 alloc，覆盖 8192 字节。
        unsafe { big.write_bytes(0x5A, 8192) };
        assert_eq!(dealloc(big, 8192, 16), 0);

        // dealloc 入口校验：null ptr → EFAULT；size == 0 / 非法 layout → EINVAL。
        assert_eq!(dealloc(core::ptr::null_mut(), 16, 8), Errno::EFAULT.code());
        assert_eq!(dealloc(ptr, 0, 8), Errno::EINVAL.code());
        assert_eq!(dealloc(ptr, 16, 3), Errno::EINVAL.code());
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

    // —— Component endpoints（Contract / Endpoint）导出面 ——

    /// Contract / Endpoint 导出面：create 期间 staged publish（返回 0，无 id）→
    /// create 返回 0 后 Core 提交 → `kcore_endpoint_lookup` 按
    /// `(provider, port_name, contract)` 解析到 EndpointId。
    #[test]
    fn endpoint_publish_stages_and_lookup_resolves_after_commit() {
        use crate::component::{containment, endpoint, registry};

        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        endpoint::init();

        const CONTRACT: u64 = 0xE0D0_1001;
        const ABI: u64 = 0xE0D0_1002;

        // Given：一个正在 create（Starting）的实例。
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    crate::component::endpoint::ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };

        // When：create 执行期间经导出发布（staged）。
        let staged = containment::with_test_init_boundary(Some(id), || {
            kcore_endpoint_publish(
                b"blk0".as_ptr(),
                4,
                CONTRACT,
                0,
                ABI,
                7,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
        });
        assert_eq!(staged, 0, "staged publish 返回 0（id 只在 commit 后存在）");

        // Then：commit 之前 lookup 不可见（不交付半成品）。
        let mut out = 0u64;
        assert_eq!(
            kcore_endpoint_lookup(id.raw(), b"blk0".as_ptr(), 4, CONTRACT, &mut out),
            Errno::ENOENT.code()
        );

        // When：Core 在 create 返回 0 后提交 pending endpoints 并 finish_start。
        {
            let mut reg = registry::get_registry().lock();
            endpoint::get_endpoints()
                .lock()
                .commit_pending(&reg, id)
                .unwrap();
            reg.finish_start(id).unwrap();
        }

        // Then：lookup 解析到单调 EndpointId（从 1 起）。
        assert_eq!(
            kcore_endpoint_lookup(id.raw(), b"blk0".as_ptr(), 4, CONTRACT, &mut out),
            0
        );
        assert!(out >= 1, "EndpointId 从 1 起且单调");
    }

    /// Endpoint 发布是 create-time 操作：普通任务边界与 destroy（exit）边界都
    /// 不是合法 principal → `-EPERM`（`ambient_init` 门禁）。
    #[test]
    fn endpoint_publish_without_init_principal_is_rejected() {
        use crate::component::containment;
        use crate::task::TaskId;

        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(7), ComponentId::from_raw(3));

        // When / Then：任务边界（非 init）→ EPERM。
        assert_eq!(
            kcore_endpoint_publish(
                b"blk0".as_ptr(),
                4,
                1,
                0,
                1,
                0,
                core::ptr::null(),
                core::ptr::null_mut()
            ),
            Errno::EPERM.code()
        );
        // destroy 钩子（Exit 边界）同样不是 publish principal。
        containment::with_test_exit_boundary(ComponentId::from_raw(9), || {
            assert_eq!(
                kcore_endpoint_publish(
                    b"blk0".as_ptr(),
                    4,
                    1,
                    0,
                    1,
                    0,
                    core::ptr::null(),
                    core::ptr::null_mut()
                ),
                Errno::EPERM.code()
            );
        });

        containment::enter_anchor();
    }

    /// `kcore_endpoint_call` 的 ABI 边界：frame 结构非法（空 `out_status` /
    /// 长度非零配空指针）→ `-EFAULT`，且在 caller 解析之前（与其它导出的 out
    /// 指针检查同一顺序；因此不需要任何执行边界）。
    #[test]
    fn endpoint_call_export_maps_invalid_frame_to_efault() {
        // out_status 为空。
        assert_eq!(
            kcore_endpoint_call(
                1,
                0,
                core::ptr::null(),
                0,
                core::ptr::null(),
                0,
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut(),
            ),
            Errno::EFAULT.code()
        );
        // 非空长度配空指针（args）。
        let mut status = 0i32;
        assert_eq!(
            kcore_endpoint_call(
                1,
                0,
                core::ptr::null(),
                3,
                core::ptr::null(),
                0,
                core::ptr::null_mut(),
                0,
                &mut status,
            ),
            Errno::EFAULT.code()
        );
        assert_eq!(status, 0, "失败调用不写 out_status");
    }

    /// `kcore_endpoint_lookup` 的错误约定：out 为空 `EFAULT`；名字非法 `EINVAL`；
    /// 未发布的名字 / 未知 provider `ENOENT`；契约不符 `EINVAL`。
    #[test]
    fn endpoint_lookup_maps_missing_name_and_contract_to_errno() {
        use crate::component::{endpoint, registry};

        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        endpoint::init();

        const CONTRACT: u64 = 0xE0D0_2001;
        const OTHER_CONTRACT: u64 = 0xE0D0_2002;
        const ABI: u64 = 0xE0D0_2003;

        // Given：一个 Ready provider 已发布 blk0。
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    crate::component::endpoint::ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        {
            let reg = registry::get_registry().lock();
            let mut eps = endpoint::get_endpoints().lock();
            eps.stage_publish(
                &reg,
                id,
                b"blk0",
                ContractId::from_raw(CONTRACT),
                InterfaceKind::Device,
                InterfaceAbi::from_raw(ABI),
                7,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
            eps.commit_pending(&reg, id).unwrap();
        }

        let mut out = 0u64;
        // 空 out → EFAULT（早于解析）。
        assert_eq!(
            kcore_endpoint_lookup(
                id.raw(),
                b"blk0".as_ptr(),
                4,
                CONTRACT,
                core::ptr::null_mut()
            ),
            Errno::EFAULT.code()
        );
        // 名字非法 → EINVAL（早于解析）。
        assert_eq!(
            kcore_endpoint_lookup(id.raw(), core::ptr::null(), 0, CONTRACT, &mut out),
            Errno::EINVAL.code()
        );
        // 未发布的名字 → ENOENT。
        assert_eq!(
            kcore_endpoint_lookup(id.raw(), b"missing".as_ptr(), 7, CONTRACT, &mut out),
            Errno::ENOENT.code()
        );
        // 契约不符 → EINVAL。
        assert_eq!(
            kcore_endpoint_lookup(id.raw(), b"blk0".as_ptr(), 4, OTHER_CONTRACT, &mut out),
            Errno::EINVAL.code()
        );
        // 未知 provider → ENOENT（名字表按 provider 隔离 = 未发布）。
        assert_eq!(
            kcore_endpoint_lookup(0xDEAD, b"blk0".as_ptr(), 4, CONTRACT, &mut out),
            Errno::ENOENT.code()
        );
    }

    /// `kcore_endpoint_validate`：只读核对已持有的 id——**contract + abi 都
    /// exact-match**（发现路径不校验 abi，consumer 用它补齐）+ 存活。
    /// 无副作用：失败不污染后续调用。
    #[test]
    fn endpoint_validate_checks_contract_abi_and_liveness() {
        use crate::component::{endpoint, registry};

        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        endpoint::init();

        const CONTRACT: u64 = 0xE0D0_4001;
        const OTHER_CONTRACT: u64 = 0xE0D0_4002;
        const ABI: u64 = 0xE0D0_4003;
        const OTHER_ABI: u64 = 0xE0D0_4004;

        // Given：一个 Ready provider 已发布 val0 并解析出 EndpointId。
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    crate::component::endpoint::ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        {
            let reg = registry::get_registry().lock();
            let mut eps = endpoint::get_endpoints().lock();
            eps.stage_publish(
                &reg,
                id,
                b"val0",
                ContractId::from_raw(CONTRACT),
                InterfaceKind::Device,
                InterfaceAbi::from_raw(ABI),
                7,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
            eps.commit_pending(&reg, id).unwrap();
        }
        let mut out = 0u64;
        assert_eq!(
            kcore_endpoint_lookup(id.raw(), b"val0".as_ptr(), 4, CONTRACT, &mut out),
            0
        );
        let ep = out;

        // When / Then：contract + abi 都匹配 → 0。
        assert_eq!(kcore_endpoint_validate(ep, CONTRACT, ABI), 0);
        // 任一不符 → EINVAL（validate 是唯一补齐 abi 校验的入口）。
        assert_eq!(
            kcore_endpoint_validate(ep, OTHER_CONTRACT, ABI),
            Errno::EINVAL.code()
        );
        assert_eq!(
            kcore_endpoint_validate(ep, CONTRACT, OTHER_ABI),
            Errno::EINVAL.code()
        );
        // 未知 / 未发布 id → ENOENT。
        assert_eq!(
            kcore_endpoint_validate(0xDEAD, CONTRACT, ABI),
            Errno::ENOENT.code()
        );
        // 只读：失败的核对不改变 endpoint 存活（随后仍返回 0）。
        assert_eq!(kcore_endpoint_validate(ep, CONTRACT, ABI), 0);

        // owner 离开 Ready（停止 / 失败）→ 死端点 ENOENT（与 lookup 同一存活档位）。
        {
            let mut reg = registry::get_registry().lock();
            reg.begin_stop(id).unwrap();
        }
        assert_eq!(
            kcore_endpoint_validate(ep, CONTRACT, ABI),
            Errno::ENOENT.code()
        );
    }

    /// `kcore_endpoint_bind`：同域 KernelNative → **DIRECT**，`api` / `ctx` 原样交付
    /// （Core 不解引用）；contract / abi 必须 exact-match（与 validate 同源）。
    #[test]
    fn endpoint_bind_selects_direct_and_hands_out_api_ctx() {
        use crate::component::containment;
        use crate::component::{endpoint, registry};
        use crate::generated::abi::KCORE_ENDPOINT_MECHANISM_DIRECT;
        use crate::task::TaskId;

        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        endpoint::init();

        // 契约 id 在**全局** endpoint 注册表里是唯一真相：取一个未被其它用例占用
        // 的区间（重复 id + 不同 abi = commit AbiMismatch，跨用例串扰）。
        const CONTRACT: u64 = 0xE0D0_6001;
        const OTHER_CONTRACT: u64 = 0xE0D0_6002;
        const ABI: u64 = 0xE0D0_6003;
        const OTHER_ABI: u64 = 0xE0D0_6004;
        const CALLER: ComponentId = ComponentId::from_raw(0x00C0_FFEE);

        // Given：一个 Ready provider，发布时交付了 Direct function table + state。
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    crate::component::endpoint::ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            id
        };
        let mut state = 0u8;
        let ctx = &mut state as *mut u8 as *mut ();
        let api = &TABLE as *const u8 as *const ();
        {
            let reg = registry::get_registry().lock();
            let mut eps = endpoint::get_endpoints().lock();
            eps.stage_publish(
                &reg,
                id,
                b"blk0",
                ContractId::from_raw(CONTRACT),
                InterfaceKind::Device,
                InterfaceAbi::from_raw(ABI),
                7,
                api,
                ctx,
            )
            .unwrap();
            eps.commit_pending(&reg, id).unwrap();
        }
        let mut out = 0u64;
        assert_eq!(
            kcore_endpoint_lookup(id.raw(), b"blk0".as_ptr(), 4, CONTRACT, &mut out),
            0
        );
        let ep = out;

        // Given：一个组件 caller 边界（机制选择需要 caller 的执行域）。
        containment::enter_anchor();
        containment::enter_task(TaskId::from_raw(7), CALLER);

        // When：bind。
        let (mut mechanism, mut bound_api, mut bound_ctx) = (0u32, 0usize, 0usize);
        assert_eq!(
            kcore_endpoint_bind(
                ep,
                CONTRACT,
                ABI,
                &mut mechanism,
                &mut bound_api,
                &mut bound_ctx
            ),
            0
        );

        // Then：机制 = DIRECT；api / ctx 是发布时的原值。
        assert_eq!(mechanism, KCORE_ENDPOINT_MECHANISM_DIRECT);
        assert_eq!(bound_api, api as usize);
        assert_eq!(bound_ctx, ctx as usize);

        // contract / abi 不符 → EINVAL；未知 id → ENOENT（与 validate 同档）。
        assert_eq!(
            kcore_endpoint_bind(
                ep,
                OTHER_CONTRACT,
                ABI,
                &mut mechanism,
                &mut bound_api,
                &mut bound_ctx
            ),
            Errno::EINVAL.code()
        );
        assert_eq!(
            kcore_endpoint_bind(
                ep,
                CONTRACT,
                OTHER_ABI,
                &mut mechanism,
                &mut bound_api,
                &mut bound_ctx
            ),
            Errno::EINVAL.code()
        );
        assert_eq!(
            kcore_endpoint_bind(
                0xDEAD,
                CONTRACT,
                ABI,
                &mut mechanism,
                &mut bound_api,
                &mut bound_ctx
            ),
            Errno::ENOENT.code()
        );

        // out 指针：任一为空 → EFAULT（早于 caller 解析）。
        assert_eq!(
            kcore_endpoint_bind(
                ep,
                CONTRACT,
                ABI,
                core::ptr::null_mut(),
                &mut bound_api,
                &mut bound_ctx
            ),
            Errno::EFAULT.code()
        );
        assert_eq!(
            kcore_endpoint_bind(
                ep,
                CONTRACT,
                ABI,
                &mut mechanism,
                core::ptr::null_mut(),
                &mut bound_ctx
            ),
            Errno::EFAULT.code()
        );
        assert_eq!(
            kcore_endpoint_bind(
                ep,
                CONTRACT,
                ABI,
                &mut mechanism,
                &mut bound_api,
                core::ptr::null_mut()
            ),
            Errno::EFAULT.code()
        );

        containment::enter_anchor();
    }

    /// bind 需要 caller 的执行域：不在任何组件执行边界内 → `-EPERM`（机制选择
    /// 无从谈起，绝不默认成 Direct）。
    #[test]
    fn endpoint_bind_without_component_caller_is_rejected() {
        use crate::component::containment;

        let _boundary = containment::test_boundary_lock();
        crate::sched::init();
        crate::task::init();
        crate::component::registry::init();
        containment::enter_anchor();

        // 无边界、无运行任务、无 loader 身份 → `ambient()` 为 None：bind 必须
        // `-EPERM`（机制选择需要 caller 的执行域，绝不默认成 Direct）。并行测试
        // 共享这些进程全局量，因此仅在确认没有 transient 活跃身份时断言。
        if crate::sched::current_task().is_none()
            && crate::component::load::current_component().is_none()
        {
            let (mut mechanism, mut api, mut ctx) = (0u32, 0usize, 0usize);
            assert_eq!(
                kcore_endpoint_bind(1, 1, 1, &mut mechanism, &mut api, &mut ctx),
                Errno::EPERM.code()
            );
        }
        containment::enter_anchor();
    }

    /// create 失败 / panic（load.rs 共用的 `fail_component` 清理路径）：pending
    /// endpoint 被丢弃，`kcore_endpoint_lookup` 找不到任何东西——半成品绝不浮出，
    /// 事后提交也不可能产出（provider 已 `Failed`）。
    #[test]
    fn failed_create_discards_staged_endpoint_and_lookup_finds_nothing() {
        use crate::component::{containment, endpoint, registry};

        let _boundary = containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        endpoint::init();

        const CONTRACT: u64 = 0xE0D0_3001;
        const ABI: u64 = 0xE0D0_3002;

        // Given：一个正在 create 的实例，已在 create 期间 staged publish。
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"export-test",
                    crate::component::registry::test_support::test_loaded(0, None),
                    crate::component::endpoint::ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };
        assert_eq!(
            containment::with_test_init_boundary(Some(id), || {
                kcore_endpoint_publish(
                    b"blk0".as_ptr(),
                    4,
                    CONTRACT,
                    0,
                    ABI,
                    7,
                    core::ptr::null(),
                    core::ptr::null_mut(),
                )
            }),
            0
        );

        // When：create 失败（失败返回与 panic 分支共用这条清理）。
        crate::component::failure::fail_component(
            id,
            crate::component::load::ComponentLoadError::CreateFailed(1),
        );

        // Then：lookup 找不到；事后提交被 `ProviderNotReady` 拒绝。
        let mut out = 0u64;
        assert_eq!(
            kcore_endpoint_lookup(id.raw(), b"blk0".as_ptr(), 4, CONTRACT, &mut out),
            Errno::ENOENT.code()
        );
        let reg = registry::get_registry().lock();
        assert_eq!(
            endpoint::get_endpoints().lock().commit_pending(&reg, id),
            Err(crate::component::endpoint::EndpointError::ProviderNotReady)
        );
    }
}
