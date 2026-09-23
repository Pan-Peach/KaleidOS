//! 结构化 Trace —— 断言与未来确定性重放的证据基础（见 docs/development/testing.md）。
//!
//! 记录形态：`TraceRecord { seq, timestamp, event }`，
//! `seq` 为 Core 分配的单调序号（**断言排序依据**；`timestamp` 只作元数据）；
//! `event` 为类型化事件（task switch / grant / revoke / 组件生命周期 /
//! IRQ / policy proposal / Core rejection），含类型化 ID 与理由，
//! 不用格式化字符串或指针。
//!
//! 目标端用固定容量环形缓冲；溢出必须显式标记（[`TraceStats`] 的
//! `overwritten_total`），reader 的真实缺口 = `returned_seq - requested_seq`，
//! 不允许静默丢事件导致断言误导。
//!
//! 第一阶段只实现一条链：
//! `Core event producer → fixed-size TraceRing → Inspector / Benchmark / CoreTest reader`。
//! 运行时过滤是最小的 11 位事件使能掩码（Core 管理路径 / Monitor 控制，见
//! [`ring`]）；不做动态订阅系统、filter engine、磁盘 trace、用户态 daemon。
//! ring 容量由 Kconfig `TRACE_CAPACITY` 决定（见 [`capacity`]）。
//!
//! 模块划分：
//! - [`event`]：事件类型与 payload（[`TraceEvent`]）。
//! - [`ring`]：固定容量环形缓冲与读写语义（[`emit`] / [`visit_since`]）。
//!
//! 用法（Core 内部 chokepoint 生产 → 测试 / Inspector 消费）：
//! ```ignore
//! trace::emit(TraceEvent::ComponentState { component, from, to });
//! trace::visit_since(0, |record| { /* 断言 record.event */ });
//! ```

pub mod abi;
pub mod event;
pub mod ring;

pub use abi::{TraceRecordAbi, TraceStatsAbi};
pub use event::{RejectReason, TraceEvent};
pub use ring::{ENABLED_MASK_ALL, TraceStats, capacity, clear, emit, next_seq, stats, visit_since};

/// 运行时使能掩码的管理面（Core 内部：Monitor；不跨 ABI 导出写入口）。
pub(crate) use ring::{
    MASK_COMPONENT, MASK_ENDPOINT, MASK_IRQ, MASK_POLICY, MASK_RESOURCE, MASK_TASK, enabled_mask,
    set_enabled_mask,
};

/// host 测试专用：把 ring 完整复位到"序号 1、无记录"（runtime `clear` 不回绕
/// `seq`，见 [`ring`] 的锁纪律）。
#[cfg(all(test, feature = "trace"))]
pub(crate) use ring::reset_for_test;

/// 全局 ring 的 host 测试设施（沿用 `memory::test_support` / `machine::test_support`
/// 的约定）。只有 trace 编译进来时才有发射可断言。
#[cfg(all(test, feature = "trace"))]
pub(crate) mod test_support {
    use super::TraceEvent;
    use crate::test_support::{Rank, TestLock};

    /// 串行化会 `clear()` / 需要独占读窗口的测试（ring 自身与 Inspector 读侧）。
    ///
    /// rank = TRACE（规范顺序 `SCHED → LOAD → INSPECTOR → IRQ → TIMER → BOUNDARY → MACHINE → MEMORY → TRACE`；见
    /// [`crate::test_support`]）。
    pub(crate) static GUARD: TestLock = TestLock::new(Rank::Trace);

    /// 读出当前 ring 里的全部事件（`seq` 升序）。
    pub(crate) fn events() -> alloc::vec::Vec<TraceEvent> {
        let mut events = alloc::vec::Vec::new();
        super::visit_since(0, |record| events.push(record.event));
        events
    }

    /// 断言 `expected` 是 `actual` 的**子序列**（按序出现，中间可以夹别的事件）。
    ///
    /// 为什么不用"按 ComponentId 过滤再全等"：`ComponentId` 只在**单个
    /// `Registry` 实例**内唯一，而 trace ring 是进程全局的 —— 用局部 registry
    /// 的测试会和用全局 registry 的测试复用同样的编号，按 id 过滤会捞到别人的
    /// 事件。子序列匹配对并行测试的插入完全免疫，同时仍然验证**相对顺序**。
    pub(crate) fn assert_subsequence(expected: &[TraceEvent], actual: &[TraceEvent]) {
        let mut wanted = expected.iter();
        let mut next = wanted.next();
        for event in actual {
            if next == Some(event) {
                next = wanted.next();
            }
        }
        assert!(next.is_none(), "事件序列未按预期出现：缺 {next:?}");
    }
}

/// 一条结构化 trace 记录。
///
/// `seq` 是 Core 分配、单调递增的序号 —— **排序与断言的唯一依据**。
/// `timestamp` 只是元数据（`arch::TimerImpl::now()` 的原始 tick）：
/// 不同平台 / 不同 QEMU / 不同宿主之间不可直接比较，只用于成本归因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceRecord {
    pub seq: u64,
    pub timestamp: u64,
    pub event: TraceEvent,
}
