//! Resource Core —— 系统的资源权威（Reference Monitor）。
//!
//! 只保存真相（存在性/状态/所有权/生命周期），**不实现**策略算法。
//! 判断标准：如果一个完全错误的 Component 能通过某 API 破坏全局不变式，
//! 就缩小 API 或把 authority 收回 Core。见 `docs/core-philosophy.md`。

pub mod component;
pub mod handle;
pub mod irq;
pub mod memory;
pub mod object;
pub mod task;
pub mod timer;
pub mod trace;