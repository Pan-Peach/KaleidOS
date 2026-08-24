//! CoreTest 测试组件：在 QEMU / 真实硬件上验证 Core 与 Arch 的真实行为（见 `docs/testing.md`）。
//! - 验证：帧所有权、任务状态转换、地址空间映射、timer、IRQ、handle 生命周期、资源回收
//! - 对抗性测试：double free / wrong owner / stale handle / invalid transition / duplicate claim / illegal map / invalid proposal
//! - 约束：无 god-mode —— 只走真实 Core API（最多只读 TestInspector）
//! - 报告（Oracle）：静态用例表 + 注入 `core::fmt::Write` 输出 TEST START/PASS/FAIL/SUMMARY。

#![no_std]

#[cfg(test)]
extern crate std;