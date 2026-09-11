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
//! | Component lifecycle（v2） | `kcore_component_load` `kcore_interface_publish` `kcore_interface_available` | 组件加载/接口发布的**语义入口**（非裸 registry mutation；requester/provider 由 Core 从 call_init 上下文解析，不信任组件自报身份） |
//! | Task control（v2） | `kcore_task_create` `kcore_task_start` `kcore_task_yield` `kcore_task_exit` `kcore_task_state` | 任务生命周期的**语义入口**（entry 必须落在 requester 组件镜像内；状态推进过 Core 状态机验证） |
//! | Scheduler（v2） | `kcore_sched_run` | 把 CPU 交给调度器（propose → validate → commit → switch 全在 Core） |
//! | Resource authority（v3 起步） | `kcore_mmio_claim` `kcore_mmio_read_u32` `kcore_irq_claim` `kcore_irq_register` `kcore_irq_enable` | 设备认领 + 单次 MMIO 读 + 设备中断线认领/注册/使能：claim = request → Core authorize → grant（authorize phase 1 恒 allow，见 `handle/mmio.rs`、`handle/irq.rs`）；read = 每次调用 Core 重新验证 handle 后才访问硬件。组件拿到的只是 raw handle，**不是地址/中断号**；全部 `0 / -Errno`、值走 out 参数 |
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
//! # 明确不导出（未经 Core validation 的裸 authority mutation）
//!
//! 组件可以 **request** 资源（v3 的 `kcore_mmio_claim` = request → Core
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
//!   `kcore_component_load` 是完整语义请求（store → loader → declare → resolve
//!   → start → call_init）。
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

use crate::component::interface::{InterfaceKind, InterfaceVersion, get_interfaces};
use crate::component::registry;
use crate::errno::{Errno, status};
use crate::handle::{irq, mmio};
use crate::machine;
use crate::memory;
use crate::sched;
use crate::task::{self, TaskId, TaskState};
use arch::{Console, ConsoleImpl};
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
// Category 3：Machine query（已提交机器真相的只读查询；counts/ids → u32）
// ---------------------------------------------------------------------------

extern "C" fn kcore_machine_boot_hart() -> u32 {
    machine::committed().map_or(0, |m| m.boot_hart as u32)
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

/// 请求 Core 加载并启动组件（store → loader → registry → call_init 全链，
/// 与 monitor `load` 同源）。返回 ComponentId raw（≥ 0）/ `-Errno`
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

/// 发布接口。provider = 当前正在初始化的组件（Core 记录，**不信任组件自报
/// 身份**）。返回 BindingId raw（≥ 0）/ `-Errno`（`EINVAL` 名字/kind 非法；
/// `EPERM` 不在组件 init 上下文；其余见 `Errno::from(InterfaceError)`）。
extern "C" fn kcore_interface_publish(
    name_ptr: *const u8,
    name_len: usize,
    kind: u32,
    version: u32,
    context: *mut (),
) -> i32 {
    let Some(name) = checked_name(name_ptr, name_len) else {
        return Errno::EINVAL.code();
    };
    let Some(kind) = kind_from_u32(kind) else {
        return Errno::EINVAL.code();
    };
    let Some(provider) = crate::component::load::current_component() else {
        return Errno::EPERM.code();
    };
    let reg = registry::get_registry().lock();
    let mut ifs = get_interfaces().lock();
    match ifs.publish(
        &reg,
        provider,
        name,
        kind,
        InterfaceVersion::from_raw(version),
        context,
    ) {
        Ok(binding) => binding.raw() as i32,
        Err(error) => Errno::from(error).code(),
    }
}

/// 只读查询：`(name, kind, version)` 是否已绑定且 provider 存活（Ready）。
/// 1 = 可用（可 resolve），0 = 不可用。
extern "C" fn kcore_interface_available(
    name_ptr: *const u8,
    name_len: usize,
    kind: u32,
    version: u32,
) -> i32 {
    let (Some(name), Some(kind)) = (checked_name(name_ptr, name_len), kind_from_u32(kind)) else {
        return 0;
    };
    let reg = registry::get_registry().lock();
    let ifs = get_interfaces().lock();
    ifs.resolve(&reg, name, kind, InterfaceVersion::from_raw(version))
        .is_ok() as i32
}

// ---------------------------------------------------------------------------
// Category 6：Task control（v2；语义入口，authority 校验在 Core）
// ---------------------------------------------------------------------------

/// 解析 Core API caller：运行任务用 TaskRecord.owner；锚点上的组件 init
/// 用 loader 记录的 call_init 身份。
fn current_task_requester() -> Option<crate::component::ComponentId> {
    task::current_owner().or_else(crate::component::load::current_component)
}

/// 创建任务。requester = 当前 caller；`entry` 必须落在该组件的
/// 装载镜像内（越界指针一律拒绝）。返回 TaskId raw（≥ 0）/ `-Errno`
/// （`EPERM` 无法解析 caller；其余见 `Errno::from(TaskError)`）。
extern "C" fn kcore_task_create(entry: usize) -> i32 {
    let Some(requester) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    match task::create_task(requester, entry) {
        Ok(id) => id.raw() as i32,
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

/// 认领一台已发现设备的 MMIO authority（C6 起步）。
///
/// 语义：compatible 匹配 `MachineInfo.devices` → Core authorize（phase 1 恒
/// allow）→ 独占检查（设备已归其他 owner 则拒绝）→ grant `MmioHandle`。
/// 成功 = 0，raw handle（`to_raw` 编码：高 32 位 slot、低 32 位 generation；
/// **不是地址**）写入 `*out_handle`（调用方保证可写，任意对齐）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EINVAL` 名字非法 / `EPERM` 无法解析
/// caller 或 Core 策略拒绝 / `ENODEV` 无匹配设备 / `EBUSY` 匹配设备全被认领）。
extern "C" fn kcore_mmio_claim(name_ptr: *const u8, name_len: usize, out_handle: *mut u64) -> i32 {
    if out_handle.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(compatible) = checked_name(name_ptr, name_len) else {
        return Errno::EINVAL.code();
    };
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    match mmio::claim(caller, compatible) {
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
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    match mmio::read_u32(caller, mmio::MmioHandle::from_raw(handle), offset) {
        Ok(value) => {
            // SAFETY: 同 `kcore_mmio_claim`（调用方保证可写；unaligned 写防未对齐 UB）。
            unsafe { core::ptr::write_unaligned(out_value, value) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
}

// ---------------------------------------------------------------------------
// Category 8（续）：IRQ authority（v3 起步；claim → register → enable）
// ---------------------------------------------------------------------------

/// 认领一台已发现设备的中断线（C6 骨架）。
///
/// 语义：compatible 匹配 `MachineInfo.devices` → 取设备的 `irq`（PLIC global
/// interrupt id）→ Core authorize（phase 1 恒 allow）→ 独占检查（该中断号已归
/// 其他 owner 则拒绝）→ grant `IrqHandle`。
/// 成功 = 0，raw handle 写入 `*out_handle`（同 `kcore_mmio_claim` 的编码：
/// 高 32 位 slot、低 32 位 generation；**不是中断号**）；
/// 失败 = `-Errno`（`EFAULT` out 为空 / `EINVAL` 名字非法 / `EPERM` 无法解析
/// caller 或 Core 策略拒绝 / `ENODEV` 无匹配设备或设备无中断线 / `EBUSY` 中断线
/// 已被认领）。
extern "C" fn kcore_irq_claim(name_ptr: *const u8, name_len: usize, out_handle: *mut u64) -> i32 {
    if out_handle.is_null() {
        return Errno::EFAULT.code();
    }
    let Some(compatible) = checked_name(name_ptr, name_len) else {
        return Errno::EINVAL.code();
    };
    let Some(caller) = current_task_requester() else {
        return Errno::EPERM.code();
    };
    match irq::claim(caller, compatible) {
        Ok(handle) => {
            // SAFETY: 同 `kcore_mmio_claim`（调用方保证可写；unaligned 写防未对齐 UB）。
            unsafe { core::ptr::write_unaligned(out_handle, handle.to_raw()) };
            0
        }
        Err(error) => Errno::from(error).code(),
    }
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

// ---------------------------------------------------------------------------
// 导出表（v1 白名单；添加符号 = 破坏性 ABI 变更，必须同步 bump 文档）
// ---------------------------------------------------------------------------

static EXPORTS: [Export; 24] = [
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
    // Category 7：Scheduler（v2）
    Export {
        name: b"kcore_sched_run",
        address: ExportAddress(kcore_sched_run as *const ()),
    },
    // Category 8：Resource authority（v3 起步）
    Export {
        name: b"kcore_mmio_claim",
        address: ExportAddress(kcore_mmio_claim as *const ()),
    },
    Export {
        name: b"kcore_mmio_read_u32",
        address: ExportAddress(kcore_mmio_read_u32 as *const ()),
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
            &b"kcore_component_load"[..],
            &b"kcore_interface_publish"[..],
            &b"kcore_interface_available"[..],
            &b"kcore_task_create"[..],
            &b"kcore_task_start"[..],
            &b"kcore_task_yield"[..],
            &b"kcore_task_exit"[..],
            &b"kcore_task_state"[..],
            &b"kcore_sched_run"[..],
            &b"kcore_mmio_claim"[..],
            &b"kcore_mmio_read_u32"[..],
            &b"kcore_irq_claim"[..],
            &b"kcore_irq_register"[..],
            &b"kcore_irq_enable"[..],
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

    /// v3 资源 authority API 的错误约定：`0 / -Errno`，值走 out 参数。
    #[test]
    fn resource_authority_apis_follow_status_convention() {
        let claim = resolve(b"kcore_mmio_claim").unwrap();
        let claim: extern "C" fn(*const u8, usize, *mut u64) -> i32 =
            unsafe { core::mem::transmute(claim) };
        let read = resolve(b"kcore_mmio_read_u32").unwrap();
        let read: extern "C" fn(u64, u32, *mut u32) -> i32 = unsafe { core::mem::transmute(read) };

        let mut out = 0u64;
        // out 为空 → EFAULT（早于设备/硬件逻辑，host 可安全断言）
        assert_eq!(
            claim(b"virtio,mmio".as_ptr(), 11, core::ptr::null_mut()),
            -14
        );
        assert_eq!(read(0, 0, core::ptr::null_mut()), -14);
        // 名字非法 → EINVAL（早于 caller 解析与设备匹配）
        assert_eq!(claim(core::ptr::null(), 0, &mut out), -22);

        // IRQ claim 与 MMIO claim 同形（早期路径可安全断言；register/enable 需
        // 真实 caller，留给 QEMU CoreTest）。
        let irq_claim = resolve(b"kcore_irq_claim").unwrap();
        let irq_claim: extern "C" fn(*const u8, usize, *mut u64) -> i32 =
            unsafe { core::mem::transmute(irq_claim) };
        assert_eq!(
            irq_claim(b"virtio,mmio".as_ptr(), 11, core::ptr::null_mut()),
            -14
        );
        assert_eq!(irq_claim(core::ptr::null(), 0, &mut out), -22);
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
}
