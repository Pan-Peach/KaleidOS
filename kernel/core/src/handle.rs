//! Handle / Authority：类型化、不可伪造的授权。
//! FrameHandle / MmioHandle / IrqHandle / DmaHandle / TaskHandle / TimerHandle / AddressSpaceHandle。
//! 驱动永远不应拿到裸物理地址、裸 IRQ 号或裸指针。
//!
//! 与 ID（TaskId/FrameId/ComponentId，可伪造、可传递的身份标识）不同：
//! Handle 是 **Authority** —— 只能由 Core 创建与校验，不可伪造。
//! ID 可以是不可信输入；任何来自 Component / IPC / Wasm 的 ID 都必须重新经过 Core validation。
//!
//! 设计要点（实现时遵守，参照 seL4 typed capability）：
//! - Handle 只能由 Core 创建与校验，组件不能自行构造（不可伪造）；
//! - 每种资源一个类型，不同 Handle 之间不可混用；
//! - 校验路径必须记录 trace（grant / revoke / stale 拒绝）。
//!
//! 计划中的 host test（Core 实现后补齐，见 docs/testing.md 对抗性清单）：
//! - stale handle 被 Core 拒绝；
//! - wrong owner 使用 handle 被拒；
//! - revoke 后该组件全部 handle 失效；
//! - double release 被拒绝。