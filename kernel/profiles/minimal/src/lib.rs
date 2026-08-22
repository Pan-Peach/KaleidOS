//! minimal Profile —— 最终镜像 crate（composition point）。
//!
//! OS = Resource Core + Component Graph + Profile。本 crate 拥有：
//! - 启动编排（`boot/`）：`_start` → early console → FDT 解析 → MachineInfo
//!   → Core 初始化 → 驱动绑定 → 静态组件图启动（M0 验收 BOOT ... OK）；
//! - 唯一的 `#[panic_handler]` 与目标镜像配置（链接脚本、加载地址属于镜像事实）。
//!
//! 目标组成（M0–M2）：RR Scheduler + Simple Allocator + Logger + CoreTest，全部 KernelNative。
//! 后续 profile：tiny / game / unix / micro / wasm / debug。

#![no_std]

#[cfg(test)]
extern crate std;

pub mod boot;