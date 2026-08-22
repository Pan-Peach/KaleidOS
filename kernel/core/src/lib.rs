//! KaleidOS 内核 —— Resource Core（真相：存在性/状态/所有权/生命周期）。
//!
//! 依赖方向（Oracle 审查结论）：本 crate 不依赖 `fdt` / `arch` / 组件 / profiles；
//! 启动编排与机器初始化属于最终镜像（`kernel/profiles/*`）。
//! ISA 层（`kernel/arch/`）与 FDT 解析（`third_party/fdt` 子模块）是独立依赖。
//! 设计契约见 `docs/architecture.md` 与 `docs/core-philosophy.md`。

#![no_std]

#[cfg(test)]
extern crate std;

pub mod component;
pub mod handle;
pub mod inspector;
pub mod irq;
pub mod memory;
pub mod object;
pub mod task;
pub mod timer;
pub mod trace;