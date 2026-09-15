//! 固定容量 TraceRing —— 热路径 append、无分配。
//!
//! 语义要求（本模块即其实现）：
//! - 容量为常量 / build profile 选择（[`TRACE_CAPACITY`]）。
//! - 热路径 **no allocation**，append 为 O(1)。
//! - `seq` 由 Core 分配、单调递增，是断言排序的唯一依据；序号耗尽时停止记录，
//!   绝不回绕（seq 是身份，不是可回绕的槽位编号）。
//! - ring 覆盖时旧事件被逐出保留区，但 reader 必须能判断丢了多少
//!   （[`TraceStats`] 的 `overwritten_total`；reader 的真实缺口 =
//!   `returned_seq - requested_seq`），不允许静默丢弃导致断言误导。
//! - trace 本身绝不能破坏 Core invariant；锁只被 O(1) 临界区持有，任意代码
//!   （visitor / 组件回调 / 分配）一律留在锁外。
//!
//! 锁纪律（单核模型）：
//! - 进入临界区先 `IrqSaveGuard`（保存并关本地中断），再阻塞 `lock`。单 hart +
//!   协作式调度下，持锁者必然先跑完 —— 中断上下文里的 `emit` 也不会与被打断的
//!   持锁者自死锁。
//! - 生产端 `emit` 临界区只做"读时钟 + 写一条完整记录 + 推进 head/len/seq"：
//!   不分配、不打印、不回调、不嵌套 emit。
//! - 读侧 [`read_one`] 同样只拷一条记录就释放；[`visit_since`] 在此之上做
//!   **有界实时遍历**（不是原子快照）：visitor 在锁外调用，遍历期间的新写入不
//!   参与本次遍历，尚未读到的旧记录可能被覆盖（reader 通过 seq 缺口发现）。
//!
//! SMP 真正被使用前不预造 lock-free recorder。

use super::TraceRecord;
use super::event::TraceEvent;

#[cfg(not(test))]
use spin::Mutex;

#[cfg(feature = "trace")]
use arch::{Timer, TimerImpl};

#[cfg(all(feature = "trace", not(test)))]
use core::sync::atomic::{AtomicU32, Ordering};

// 目标端 ring 容量（records）由 Kconfig `TRACE_CAPACITY` 决定：
// Makefile（genmk.py → KCFG_TRACE_CAPACITY）→ 环境变量 → build.rs 校验 →
// OUT_DIR 常量。build.rs 不重新解释 `.config`；裸机构建缺值 / 越界直接报错，
// host 构建（cargo test / clippy）有显式默认。这里只消费生成常量。
include!(concat!(env!("OUT_DIR"), "/trace_capacity.rs"));

// —— 运行时事件使能位 ——
//
// 每个事件一位，bit i ↔ ABI kind i+1（`trace::abi` 的稳定标签 1..=12）。
// 类别掩码（`MASK_*`）只是若干位的并集：Monitor 用它做粗粒度开关。
// 位定义、`event_bit` 映射与 ABI 标签的一致性由 host test 锚定。

/// `TaskSwitch`（ABI kind 1）。
const BIT_TASK_SWITCH: u32 = 1 << 0;
/// `PolicyProposal`（ABI kind 2）。
const BIT_POLICY_PROPOSAL: u32 = 1 << 1;
/// `PolicyAccepted`（ABI kind 3）。
const BIT_POLICY_ACCEPTED: u32 = 1 << 2;
/// `PolicyRejected`（ABI kind 4）。
const BIT_POLICY_REJECTED: u32 = 1 << 3;
/// `ComponentState`（ABI kind 5）。
const BIT_COMPONENT_STATE: u32 = 1 << 4;
/// `ResourceGrant`（ABI kind 6）。
const BIT_RESOURCE_GRANT: u32 = 1 << 5;
/// `ResourceRevoke`（ABI kind 7）。
const BIT_RESOURCE_REVOKE: u32 = 1 << 6;
/// `InterfaceBind`（ABI kind 8）。
const BIT_INTERFACE_BIND: u32 = 1 << 7;
/// `InterfaceRefresh`（ABI kind 9）。
const BIT_INTERFACE_REFRESH: u32 = 1 << 8;
/// `IrqEnter`（ABI kind 10）。
const BIT_IRQ_ENTER: u32 = 1 << 9;
/// `IrqDispatch`（ABI kind 11）。
const BIT_IRQ_DISPATCH: u32 = 1 << 10;
/// `IrqAck`（ABI kind 12）。
const BIT_IRQ_ACK: u32 = 1 << 11;

/// 任务切换类别（Monitor `trace task on|off`）。
pub(crate) const MASK_TASK: u32 = BIT_TASK_SWITCH;
/// 调度策略提议 / 采纳 / 拒绝类别。
pub(crate) const MASK_POLICY: u32 = BIT_POLICY_PROPOSAL | BIT_POLICY_ACCEPTED | BIT_POLICY_REJECTED;
/// 组件生命周期类别。
pub(crate) const MASK_COMPONENT: u32 = BIT_COMPONENT_STATE;
/// authority 授予 / 回收类别。
pub(crate) const MASK_RESOURCE: u32 = BIT_RESOURCE_GRANT | BIT_RESOURCE_REVOKE;
/// interface 绑定 / 刷新类别。
pub(crate) const MASK_INTERFACE: u32 = BIT_INTERFACE_BIND | BIT_INTERFACE_REFRESH;
/// 外部中断 enter / dispatch / ack 类别。
pub(crate) const MASK_IRQ: u32 = BIT_IRQ_ENTER | BIT_IRQ_DISPATCH | BIT_IRQ_ACK;

/// 全部事件位（默认掩码：全开）。`TraceStats::enabled_mask` 是它的 `u64` 视图
/// （高位恒 0）；`CONFIG_TRACE=n` 时为 0。
pub const ENABLED_MASK_ALL: u64 =
    (MASK_TASK | MASK_POLICY | MASK_COMPONENT | MASK_RESOURCE | MASK_INTERFACE | MASK_IRQ) as u64;

// —— 运行时使能掩码 ——
//
// 生产端：一个 `AtomicU32`（12 个事件位，默认全开）。host 测试：线程本地，
// 与 ring 同理——`cargo test` 每个测试跑在自己的线程上，全局掩码会让"关掉
// 某事件"的测试干扰并行发射事件的其他测试。
#[cfg(all(feature = "trace", not(test)))]
static ENABLED_MASK: AtomicU32 = AtomicU32::new(ENABLED_MASK_ALL as u32);

#[cfg(all(feature = "trace", test))]
std::thread_local! {
    static ENABLED_MASK: core::cell::Cell<u32> =
        const { core::cell::Cell::new(ENABLED_MASK_ALL as u32) };
}

/// 读取当前掩码（`Relaxed`：掩码只决定采集，不发布与之关联的状态）。
#[cfg(feature = "trace")]
fn load_mask() -> u32 {
    #[cfg(not(test))]
    return ENABLED_MASK.load(Ordering::Relaxed);
    #[cfg(test)]
    return ENABLED_MASK.with(core::cell::Cell::get);
}

/// 写入新掩码（保留位恒 0），返回旧值。
#[cfg(feature = "trace")]
fn swap_mask(next: u32) -> u32 {
    let next = next & ENABLED_MASK_ALL as u32;
    #[cfg(not(test))]
    return ENABLED_MASK.swap(next, Ordering::Relaxed);
    #[cfg(test)]
    return ENABLED_MASK.with(|cell| cell.replace(next));
}

/// 当前已使能事件掩码（`u64` ABI 视图）。`CONFIG_TRACE=n` 时恒 0：编译期没有
/// trace，与"运行时过滤"是两回事。
#[cfg(feature = "trace")]
pub(crate) fn enabled_mask() -> u64 {
    u64::from(load_mask())
}

#[cfg(not(feature = "trace"))]
pub(crate) fn enabled_mask() -> u64 {
    0
}

/// 设置运行时事件使能掩码（**Core 管理路径专用**：Monitor；组件没有全局
/// trace-control authority，只能经 `kcore_trace_stats` 读）。被过滤的事件不记录、
/// **不消耗 `seq`**（过滤不是记录失败，不影响 reader 的缺口判断）。返回旧掩码。
#[cfg(feature = "trace")]
pub(crate) fn set_enabled_mask(mask: u32) -> u32 {
    swap_mask(mask)
}

/// `CONFIG_TRACE=n`：编译期就没有 trace，运行时掩码不存在（恒 0，写入无效）。
#[cfg(not(feature = "trace"))]
pub(crate) fn set_enabled_mask(_mask: u32) -> u32 {
    0
}

/// 采集状态（只读快照）。
///
/// `overwritten_total` 是**因 ring 满被逐出保留区**的记录总数（saturating），
/// 不是"某个 reader 漏掉的条数"——reader 的真实缺口是
/// `returned_seq - requested_seq`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceStats {
    /// 最旧存活记录的 `seq`；ring 为空时 == `next_seq`。
    pub oldest_seq: u64,
    /// 下一条记录将拿到的 `seq`。
    pub next_seq: u64,
    /// 因 ring 满被逐出保留区的记录总数（saturating 计数）。
    pub overwritten_total: u64,
    /// 已使能事件掩码（bit i ↔ ABI kind i+1，默认 [`ENABLED_MASK_ALL`]）；
    /// `CONFIG_TRACE=n` 时恒 0。
    pub enabled_mask: u64,
}

struct TraceRing {
    /// 槽位为空 = 尚未写过（`len` 之外的位置不参与遍历）。
    /// 用 `Option` 而不是哨兵记录：不需要 unsafe 的 `MaybeUninit`，也不会让
    /// "初始值"伪装成一条真事件。
    records: [Option<TraceRecord>; TRACE_CAPACITY],
    /// 下一次写入的槽位（ring 满时它同时就是最旧记录的位置）。
    head: usize,
    /// 存活记录数（<= 容量）。
    len: usize,
    /// 下一条记录将拿到的 `seq`（从 1 开始）。
    next_seq: u64,
    /// 因 ring 满被逐出保留区的记录总数。
    overwritten_total: u64,
}

impl TraceRing {
    const fn new() -> Self {
        Self {
            records: [None; TRACE_CAPACITY],
            head: 0,
            len: 0,
            next_seq: 1,
            overwritten_total: 0,
        }
    }

    /// 最旧存活记录的 `seq`；无记录时 == `next_seq`（没有更旧的留存）。
    fn oldest_seq(&self) -> u64 {
        self.next_seq.saturating_sub(self.len as u64)
    }

    #[cfg(feature = "trace")]
    fn push(&mut self, record: TraceRecord) {
        if self.len == TRACE_CAPACITY {
            self.overwritten_total = self.overwritten_total.saturating_add(1);
        } else {
            self.len += 1;
        }
        self.records[self.head] = Some(record);
        self.head = (self.head + 1) % TRACE_CAPACITY;
    }

    /// O(1) 读一条：返回 `seq >= since` 的最旧存活记录的一份拷贝。
    ///
    /// 调用方保证在锁内；本函数不做拷贝之外的任何事（不回调、不分配）。
    /// `since` 落在已被逐出的区间时返回当前最旧记录：reader 用
    /// `record.seq - since` 得到自己的真实缺口。
    fn read_one(&self, since: u64) -> Option<TraceRecord> {
        let oldest_seq = self.oldest_seq();
        let wanted = since.max(oldest_seq);
        if wanted >= self.next_seq {
            return None;
        }
        let oldest_slot = (self.head + TRACE_CAPACITY - self.len) % TRACE_CAPACITY;
        let slot = (oldest_slot + (wanted - oldest_seq) as usize) % TRACE_CAPACITY;
        self.records[slot]
    }

    /// 清空记录与逐出计数，但**保留 `seq`**：runtime 重开窗口不打断 reader 游标。
    fn clear_records(&mut self) {
        self.records = [None; TRACE_CAPACITY];
        self.head = 0;
        self.len = 0;
        self.overwritten_total = 0;
    }

    /// host 测试专用：完整复位（记录 + 计数 + `seq` 回到 1）。
    #[cfg(all(test, feature = "trace"))]
    fn reset(&mut self) {
        self.clear_records();
        self.next_seq = 1;
    }
}

// —— 存储 ——
//
// 目标端：单核全局 ring（生产实现）。
//
// host 测试：**线程本地** ring。`cargo test` 每个测试跑在自己的线程上，而 Core
// 的关键路径（sched / registry / handle / irq / interface）现在几乎都有探针，
// 并发测试会互相看到、甚至覆盖对方的事件 —— 任何"断言 ring 内容"或"断言丢失
// 计数"的测试都会变成 flaky。线程本地让每个测试只看到自己的事件，
// 生产实现完全不受影响。
#[cfg(not(test))]
static RING: Mutex<TraceRing> = Mutex::new(TraceRing::new());

#[cfg(test)]
std::thread_local! {
    static RING: core::cell::RefCell<TraceRing> =
        const { core::cell::RefCell::new(TraceRing::new()) };
}

/// 独占 ring（读侧 / 管理路径）。
///
/// **irq-save + 阻塞锁**：单 hart + 协作式调度下，锁只被 O(1) 临界区持有，
/// 持锁者必然先跑完 → 不会出现"生产者拿不到锁只好丢事件"。SMP 或抢占落地后，
/// 正确做法是 **per-hart ring**（生产者只碰自己 hart 的 buffer），而不是退回
/// 去丢弃事件。
#[cfg(not(test))]
fn with_ring<R>(body: impl FnOnce(&mut TraceRing) -> R) -> R {
    let _guard = crate::irq::IrqSaveGuard::new();
    body(&mut RING.lock())
}

#[cfg(test)]
fn with_ring<R>(body: impl FnOnce(&mut TraceRing) -> R) -> R {
    RING.with(|cell| body(&mut cell.borrow_mut()))
}

/// 读一条：O(1)，锁内只做"定位 + 拷贝一条记录"，出锁后才返回。
///
/// 这是 reader 的最小步进原语：[`visit_since`] 用它做有界遍历，
/// `kcore_trace_read` 用它实现"一次读一条"。锁内没有 visitor / 分配 / 格式化。
pub(crate) fn read_one(since: u64) -> Option<TraceRecord> {
    with_ring(|ring| ring.read_one(since))
}

/// `TraceEvent` → 使能位（显式映射；新增事件必须同时加位与 ABI 标签）。
/// 只在 `trace` feature 下被 [`event_enabled`] 使用。
#[cfg(feature = "trace")]
const fn event_bit(event: TraceEvent) -> u32 {
    match event {
        TraceEvent::TaskSwitch { .. } => BIT_TASK_SWITCH,
        TraceEvent::PolicyProposal { .. } => BIT_POLICY_PROPOSAL,
        TraceEvent::PolicyAccepted { .. } => BIT_POLICY_ACCEPTED,
        TraceEvent::PolicyRejected { .. } => BIT_POLICY_REJECTED,
        TraceEvent::ComponentState { .. } => BIT_COMPONENT_STATE,
        TraceEvent::ResourceGrant { .. } => BIT_RESOURCE_GRANT,
        TraceEvent::ResourceRevoke { .. } => BIT_RESOURCE_REVOKE,
        TraceEvent::InterfaceBind { .. } => BIT_INTERFACE_BIND,
        TraceEvent::InterfaceRefresh { .. } => BIT_INTERFACE_REFRESH,
        TraceEvent::IrqEnter { .. } => BIT_IRQ_ENTER,
        TraceEvent::IrqDispatch { .. } => BIT_IRQ_DISPATCH,
        TraceEvent::IrqAck { .. } => BIT_IRQ_ACK,
    }
}

/// 事件是否使能：`Relaxed` 原子 load + 位测试。
///
/// 被过滤的事件不记录、**不消耗 `seq`**：过滤不是记录失败，不影响 reader 的
/// 缺口判断。
#[cfg(feature = "trace")]
fn event_enabled(event: TraceEvent) -> bool {
    load_mask() & event_bit(event) != 0
}

// host 测试：本线程 `emit` 读到时钟的次数（验证被禁用的事件不碰时钟）。
#[cfg(all(test, feature = "trace"))]
std::thread_local! {
    static CLOCK_READS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

#[cfg(all(test, feature = "trace"))]
fn clock_reads() -> u64 {
    CLOCK_READS.with(core::cell::Cell::get)
}

/// 生产端：记录一个事件（热路径，O(1)、无分配、关中断串行 append）。
///
/// 由 Core 内部的 chokepoint 调用。`seq` 由这里分配；`timestamp` 只是元数据
/// （取自 `arch::TimerImpl::now()`），**排序一律用 `seq`**。
///
/// **开关**：`CONFIG_TRACE=n` 时本函数是内联空操作。事件参数都是纯值构造
/// （无副作用），所以 `#[inline(always)]` + 空函数体让编译器把整个调用连参数
/// 一起消除——热路径零成本，也不会读时钟。ring 存储仍常驻（未被写入），
/// 读侧 API 保持可用（返回空）。
///
/// **运行时过滤**：先查在 [`ENABLED_MASK_ALL`] 掩码里的使能位，**在关中断、
/// 加锁、读时钟之前**返回。这是原子 load + 分支——**不是零开销**；将来若有
/// 事件需要昂贵的 payload 准备，检查必须前置到准备之前。
#[cfg(feature = "trace")]
pub fn emit(event: TraceEvent) {
    if !event_enabled(event) {
        return;
    }
    with_ring(|ring| {
        // 序号耗尽（实际不可达）：停止记录，绝不回绕。
        let Some(next_seq) = ring.next_seq.checked_add(1) else {
            return;
        };
        #[cfg(test)]
        CLOCK_READS.with(|count| count.set(count.get() + 1));
        let record = TraceRecord {
            seq: ring.next_seq,
            timestamp: TimerImpl::now(),
            event,
        };
        ring.push(record);
        ring.next_seq = next_seq;
    });
}

/// `CONFIG_TRACE=n`：零成本空操作（见上）。
#[cfg(not(feature = "trace"))]
#[inline(always)]
pub fn emit(_event: TraceEvent) {}

/// 读侧：按 seq 升序遍历 `seq >= since` 的存活记录（只读，不修改任何状态）。
///
/// **有界实时遍历，不是原子快照**：进入时捕获排他终点 `end = next_seq()`，
/// 只遍历 `seq < end` 的记录；遍历期间的新写入不参与本次遍历。ring 覆盖会让
/// 尚未读到的旧记录消失——reader 用 seq 缺口发现（真实缺口 =
/// `record.seq - 请求的 since`），且循环严格前进（每条至少 +1，不会死循环）。
///
/// visitor 在锁**外**调用：它可以安全地 `emit`、查 [`stats`] 或分配内存。
pub fn visit_since(since: u64, mut visitor: impl FnMut(&TraceRecord)) {
    let end = next_seq();
    let mut cursor = since;
    while cursor < end {
        let Some(record) = read_one(cursor) else {
            break;
        };
        cursor = record.seq.saturating_add(1);
        visitor(&record);
    }
}

/// 下一条记录将拿到的 `seq`（reader 增量轮询用的续读游标）。
pub fn next_seq() -> u64 {
    with_ring(|ring| ring.next_seq)
}

/// 采集状态（[`TraceStats`]）。
pub fn stats() -> TraceStats {
    with_ring(|ring| TraceStats {
        oldest_seq: ring.oldest_seq(),
        next_seq: ring.next_seq,
        overwritten_total: ring.overwritten_total,
        enabled_mask: enabled_mask(),
    })
}

/// ring 容量（records）。
pub const fn capacity() -> usize {
    TRACE_CAPACITY
}

/// 清空记录与逐出计数，但**不重置 `seq`**：下一个事件继续递增。
///
/// reader 已持有的游标不会失效；若它请求的 seq 已被清空或逐出，通过
/// `record.seq - 请求的 seq` 看到缺口。
pub fn clear() {
    with_ring(|ring| ring.clear_records());
}

/// host 测试专用：完整复位到"序号 1、无记录、掩码全开"。
///
/// **不是 runtime API**：回绕 `seq` 会让已持有游标的 reader 失去对齐。
#[cfg(all(test, feature = "trace"))]
pub(crate) fn reset_for_test() {
    with_ring(|ring| ring.reset());
    set_enabled_mask(ENABLED_MASK_ALL as u32);
}

// ring 的 host 测试全部关于**真实发射**（emit + 读回）：`CONFIG_TRACE=n` 时
// emit 是空操作，这些断言没有意义，随 feature 一起编译掉。
#[cfg(all(test, feature = "trace"))]
mod tests;
