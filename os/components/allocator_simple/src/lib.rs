//! 简单帧分配器组件 —— 策略 Component（M2）。
//! 保存 free list；提议"分配 Frame #X"，由 Core 验证后 commit ownership。
//! 自身状态可重建：重置后从 Core 的帧真相重新构造。

#![no_std]

#[cfg(test)]
extern crate std;
