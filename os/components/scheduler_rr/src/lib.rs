//! RR（轮转）调度器组件 —— 策略 Component（M2）。
//! 保存 runqueue / RR cursor；提议"运行 Task #X"，由 Core 验证后 commit。
//! 自身状态可重建：重置后从 Core 的 Runnable 真相重新扫描。

#![no_std]

#[cfg(test)]
extern crate std;