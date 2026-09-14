//! driver_prober —— 协议无关的设备 prober（组件级“总线”）。
//!
//! # 职责与刻意分层
//!
//! ```text
//! prober（本组件）:  按 compatible 枚举候选 → 请求 Core 加载候选驱动一次
//!                     → 持有 assignment cursor，向驱动交付 (attempt, DeviceId)
//! driver:            自己的 init 上下文：next_assignment → claim → fine protocol
//!                    match → 接受 / 释放 → report_attempt
//! ```
//!
//! **prober 不含任何协议知识，也绝不 claim MMIO：**
//!
//! - 不包含 VirtIO 偏移、DeviceID 取值，或任何 MMIO 读；
//! - `compatible` 对它是**不透明路由键**——只按字节相等匹配，绝不解释；
//! - 最后的硬件匹配必须在驱动代码运行、且驱动持有 authority 之后才能完成，
//!   所以这里只做 coarse candidate match（见 docs/driver-model.md §9.1 / §12 Q1）。
//!
//! # 唯一通道是 assignment Service
//!
//! `driver.prober` 只传**数据**（device id / attempt），**从不携带 authority**：
//! prober 不 claim / release 任何资源，也不替驱动创建任务；驱动在**自己的**
//! `kcomp_init` 上下文里向 Core claim（Core 的 principal 规则把嵌套 init 归属到
//! 驱动本身，不是发起 load 的 prober）。
//!
//! # 生命周期
//!
//! publish 是 **staged**：`kcomp_init` 期间只记录 pending，Core 在 init 返回 0 后
//! 才提交（prober → `Ready`）。消费者（驱动）只有 prober `Ready` 后才能 bind，所以
//! 本组件在 init 里创建一个**有限** dispatch 任务：monitor 在 load 提交后
//! `sched::run()` 运行它——枚举候选 compatible → 有匹配就 `kcore_component_load`
//! 候选驱动**一次** → 退出（无后台循环；重复加载 / 热插拔明确 deferred）。
//!
//! # 延期（deferred，勿在本轮长出来）
//!
//! - TODO(prober-classes): 更多设备类 / 每类优先级；当前只有一张极小静态目录。
//! - TODO(prober-multi): 同一驱动 ↔ 多台设备（组件名唯一，多实例未解决）。
//! - TODO(prober-hotplug): 热插拔 / reset / recovery；枚举只在 dispatch 时做一次。

#![no_std]

// host 测试用（`cargo test`）；裸机目标不编入。
#[cfg(test)]
extern crate std;

mod cursor;
mod directory;

#[cfg(not(test))]
mod runtime;

pub use cursor::{AssignmentCursor, ReportError};
pub use directory::{CANDIDATES, Candidate, CandidateSet};

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(compatible: &'static [u8], component: &'static [u8]) -> Candidate {
        Candidate {
            compatible,
            component,
        }
    }

    /// 目录去重：同一组件声明多个 compatible 仍只是一个候选，键取并集。
    #[test]
    fn directory_deduplicates_components_and_merges_compatibles() {
        let dir = [
            entry(b"virtio,mmio", b"virtio_blk"),
            entry(b"virtio-mmio", b"virtio_blk"), // 同组件，第二个键
            entry(b"ns16550a", b"uart"),
        ];
        let set = CandidateSet::build(&dir);
        assert_eq!(set.len(), 2, "重复组件折叠为一个候选");
        assert_eq!(set.driver(0), b"virtio_blk");
        assert_eq!(set.driver(1), b"uart");
        assert_eq!(
            set.compatibles(0),
            [b"virtio,mmio".as_slice(), b"virtio-mmio".as_slice()],
            "同组件的多个键合并保留（不重复）"
        );
        assert_eq!(set.compatibles(1), [b"ns16550a".as_slice()]);
        assert_eq!(set.index_of(b"uart"), Some(1));
        assert_eq!(set.index_of(b"missing"), None);
    }

    /// 同一 compatible 重复出现不重复入表。
    #[test]
    fn duplicate_compatible_is_stored_once() {
        let dir = [
            entry(b"virtio,mmio", b"virtio_blk"),
            entry(b"virtio,mmio", b"virtio_blk"),
        ];
        let set = CandidateSet::build(&dir);
        assert_eq!(set.len(), 1);
        assert_eq!(set.compatibles(0), [b"virtio,mmio".as_slice()]);
    }

    /// cursor 按 push 顺序逐台交付，耗尽后 `None`；驱动之间互不干扰。
    #[test]
    fn cursor_hands_out_per_driver_in_order_then_exhausts() {
        let mut cursor = AssignmentCursor::new();
        assert!(cursor.push(b"virtio_blk", 3));
        assert!(cursor.push(b"virtio_blk", 5));
        assert!(cursor.push(b"other", 9));

        assert_eq!(cursor.next(b"virtio_blk"), Some((1, 3)));
        assert_eq!(cursor.next(b"virtio_blk"), Some((2, 5)));
        assert_eq!(cursor.next(b"virtio_blk"), None, "两台都发完后耗尽");
        // 另一驱动的 cursor 独立推进（各自自己的设备序列）。
        assert_eq!(cursor.next(b"other"), Some((3, 9)));
        assert_eq!(cursor.next(b"other"), None);
        assert_eq!(cursor.next(b"unknown"), None, "未知驱动没有分配");
    }

    /// cursor 是定长静态表：超容量拒绝，不溢出。
    #[test]
    fn cursor_is_bounded() {
        let mut cursor = AssignmentCursor::new();
        for i in 0..MAX_ASSIGNMENTS {
            assert!(cursor.push(b"d", i as u32));
        }
        assert!(!cursor.push(b"d", 99), "第 MAX_ASSIGNMENTS+1 条被拒绝");
        assert_eq!(cursor.next(b"d"), Some((1, 0)));
    }

    /// attempt 只在**下发后**可上报一次；未知 / 重复 → 拒绝（stale report 防御）。
    #[test]
    fn report_records_only_handed_out_attempts_once() {
        let mut cursor = AssignmentCursor::new();
        assert!(cursor.push(b"virtio_blk", 3));
        assert_eq!(
            cursor.report(1, 0, 0),
            Err(ReportError::NotHanded),
            "还没下发就上报必须拒绝"
        );
        assert_eq!(cursor.next(b"virtio_blk"), Some((1, 3)));
        assert_eq!(cursor.report(1, 1, 0), Ok(()));
        assert_eq!(
            cursor.report(1, 0, 0),
            Err(ReportError::AlreadyReported),
            "重复上报必须拒绝"
        );
        assert_eq!(
            cursor.report(42, 0, 0),
            Err(ReportError::UnknownAttempt),
            "未知 attempt 必须拒绝"
        );
        assert!(cursor.all_reported());
    }

    /// 内置目录：`compatible` → `virtio_blk`（opaque 键；prober 不解释它）。
    #[test]
    fn default_directory_routes_virtio_mmio_to_virtio_blk() {
        assert_eq!(CANDIDATES.len(), 1);
        assert_eq!(CANDIDATES[0].compatible, b"virtio,mmio");
        assert_eq!(CANDIDATES[0].component, b"virtio_blk");
    }
}

#[cfg(test)]
use cursor::MAX_ASSIGNMENTS;
