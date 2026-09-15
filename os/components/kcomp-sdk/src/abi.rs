//! `kcore_*` 导出 ABI（EXPORT_SYMBOL 教学版）。
//!
//! 声明即契约：名字必须与 Core `component/export.rs` 的白名单逐字节一致，签名
//! 错误 = UB（loader 只按名字精确解析，不校验签名）。这里保持**全量**声明，
//! 让各组件只共用这一份；新增 Core 导出时同步加在这里。
//!
//! # Trace 支持状态（编译期 vs 运行时，组件要能分开发现）
//!
//! - **编译期**：Core 以 `CONFIG_TRACE=n` 构建时 `trace::emit` 是内联空操作，
//!   ring 恒为空 —— `kcore_trace_read` 永远 `-ENOENT`，
//!   `kcore_trace_stats` 的 `enabled_mask == 0`。这是"这台机器没带 trace"，
//!   不是"事件被过滤"。
//! - **运行时**：`enabled_mask` 报告哪些事件 kind 会被记录（bit i ↔ kind i+1，
//!   即上方 `KIND_*` 标签；12 位掩码，默认全开 = `0x0fff`，高位保留恒 0）。
//!   被过滤的事件不记录、**不消耗 `seq`**。掩码由 Core 管理路径（Monitor）
//!   控制：组件只能**读**（`kcore_trace_stats`），没有写入口。
//!
//! 读侧是**有界实时遍历，不是原子快照**：读取之间发生覆盖时，`since` 落在已
//! 逐出区间，`kcore_trace_read` 返回当前最旧存活记录 —— reader 的真实缺口 =
//! `record.seq - since`（`TraceStatsAbi::overwritten_total` 只表示 ring 因满
//! 逐出了多少条，不等于某个 reader 漏掉的条数）。

/// IRQ 投递回调：`ctx` 原样回传，Core 不解引用。
pub type IrqHandler = extern "C" fn(ctx: *mut ());

/// 一条 trace 记录的**稳定编码** —— 必须与 Core `trace::abi::TraceRecordAbi`
/// 逐字节一致（loader 只按名字解析符号，不校验签名/布局；不一致 = UB）。
///
/// `kind` 决定 `a`/`b`/`c` 的含义，缺省字段写成 [`ABSENT`]（**不是** 0）。
/// 完整的 payload 分配表见 Core `os/core/src/trace/abi.rs` 的模块文档。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceRecordAbi {
    pub seq: u64,
    pub timestamp: u64,
    pub kind: u32,
    pub flags: u32,
    pub a: u64,
    pub b: u64,
    pub c: u64,
}

/// payload 词里的"该字段不存在"哨兵。
pub const ABSENT: u64 = u64::MAX;

// 事件标签（与 Core 的稳定编号一致；只增不改）。
pub const KIND_TASK_SWITCH: u32 = 1;
pub const KIND_POLICY_PROPOSAL: u32 = 2;
pub const KIND_POLICY_ACCEPTED: u32 = 3;
pub const KIND_POLICY_REJECTED: u32 = 4;
pub const KIND_COMPONENT_STATE: u32 = 5;
pub const KIND_RESOURCE_GRANT: u32 = 6;
pub const KIND_RESOURCE_REVOKE: u32 = 7;
pub const KIND_INTERFACE_BIND: u32 = 8;
pub const KIND_INTERFACE_REFRESH: u32 = 9;
pub const KIND_IRQ_ENTER: u32 = 10;
pub const KIND_IRQ_DISPATCH: u32 = 11;
pub const KIND_IRQ_ACK: u32 = 12;

/// 布局指纹：与 Core 的定义必须一致。改了字段就改这里的数字 —— 不一致时
/// **编译期**报错，而不是在板上以 UB 的形式出现。
const _: () = {
    assert!(core::mem::size_of::<TraceRecordAbi>() == 48);
    assert!(core::mem::align_of::<TraceRecordAbi>() == 8);
};

/// Trace 子系统状态的**稳定编码** —— 必须与 Core `trace::abi::TraceStatsAbi`
/// 逐字节一致（loader 只按名字解析符号，不校验签名/布局；不一致 = UB）。
///
/// `overwritten_total` 是**因 ring 满被逐出保留区**的记录总数，不是"某个
/// reader 漏掉的条数"——reader 的真实缺口是 `returned_seq - requested_seq`。
/// 被 `enabled_mask` 过滤的事件不记录、不消耗 `seq`，不算丢失。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceStatsAbi {
    pub capacity: u64,
    pub oldest_seq: u64,
    pub next_seq: u64,
    pub overwritten_total: u64,
    pub enabled_mask: u64,
}

/// `TraceStatsAbi` 布局指纹（与 Core 侧同一数值的编译期锚定）。
const _: () = {
    assert!(core::mem::size_of::<TraceStatsAbi>() == 40);
    assert!(core::mem::align_of::<TraceStatsAbi>() == 8);
};

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

    // -- Trace（只读观察面；支持状态见模块文档）--
    /// 读 `seq >= since` 的第一条记录到 `out`，`out_next` 回写下一次应传的
    /// `seq`。没有更多记录时返回 `-ENOENT`（**不返回 0**）。
    #[link_name = "kcore_trace_read"]
    pub fn kcore_trace_read(since: u64, out: *mut TraceRecordAbi, out_next: *mut u64) -> i32;
    /// 读 Trace 子系统状态到 `out`。成功 = 0，失败 = `-Errno`（`EFAULT` 空指针）。
    /// 字段语义见 [`TraceStatsAbi`]。
    #[link_name = "kcore_trace_stats"]
    pub fn kcore_trace_stats(out: *mut TraceStatsAbi) -> i32;

    // -- Clock（只读；组件侧计时，无 authority 语义）--
    /// 单调时钟（`rdtime` 的原始 tick）；频率见 [`kcore_timebase_hz`]。
    /// 真机 timebase 常是 10 MHz（1 tick = 100 ns）——测很短的操作要累积多次。
    #[link_name = "kcore_now"]
    pub fn kcore_now() -> u64;
    /// 时钟频率（Hz），用于把 tick 换算成时间。
    #[link_name = "kcore_timebase_hz"]
    pub fn kcore_timebase_hz() -> u64;

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
