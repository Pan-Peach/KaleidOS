//! 组件退出（graceful stop）的 Core 编排 —— **shape-only stub**。
//!
//! 与 [`super::failure`]（forced containment）对称：本模块是**优雅停止**的唯一
//! 汇合点。当前只有一个空 stub：[`stop_component`] 的**签名与调用顺序是 seam
//! 契约**（人类填空的骨架），但**每一步的语义都未定稿**——本文件不提交状态、
//! 不调用 `kcomp_exit`、不回收任何 authority。
//!
//! # 预期调用顺序（骨架，不是已实现行为）
//!
//! ```text
//! 1. registry.begin_stop(id)        Ready → Stopping：先提交"不再接受新 work"的真相
//! 2. quiesce                        publish / claim / task_create 门禁（尚未接线）
//! 3. 调用组件退出钩子 kcomp_exit     组件自己的收尾（设备 reset / mask IRQ 顺序由组件定）
//! 4. 停止 / 等待该实例的任务         当前无 task-stop API（任务下次 yield/exit 自然退出）
//! 5. Core 兜底 revoke authority      MMIO / IRQ / DMA（是否 quarantine 未定）
//! 6. 解绑 provider interfaces        含丢弃 pending publications
//! 7. registry.finish_stop(id)        Stopping → Stopped（终态）
//! 8. 实例退役 / 段内存回收            phase 1 不做（逻辑死亡、物理驻留）
//! ```
//!
//! # 明确 deferred（定稿前不要在这里长出来）
//!
//! - 退出钩子的失败 / 阻塞 / 超时 / panic 语义；
//! - 谁触发退出（组件 self-exit vs Core 发起）；
//! - "意外退出"是否独立于 `Failed` 的终态；
//! - 组件仍持有的 authority 由组件释放还是 Core 撤销；
//! - 退出钩子跑在哪条任务 / 哪个栈上；
//! - loader 是否要求 `kcomp_exit` 符号；
//! - 与"逻辑重启 = 全新实例"的关系。
//!
//! 以上问题的选项清单见 `docs/component-model.md` §5.2 —— **定稿前不要实现**。

use crate::component::ComponentId;

/// 优雅停止一个组件实例（**shape-only stub**）。
///
/// 调用顺序是 seam 契约（见模块文档），但本函数**不执行任何一步**：不提交状态、
/// 不调用 `kcomp_exit`、不回收 authority、不解绑接口；返回前不改变任何 Core 真相。
///
/// 返回类型当前是 `()`："退出是否可失败"是开放问题之一，定稿前不预设 `Result`。
///
/// TODO(component-exit): 人类填空点——先回答 `docs/component-model.md` §5.2 的
/// 开放问题，再按模块文档的顺序接线；定稿前保持 no-op。
pub fn stop_component(id: ComponentId) {
    // TODO(component-exit): 空实现占位——先定稿 docs/component-model.md §5.2，
    // 再按模块文档的顺序逐步接线（不要绕过开放问题直接写语义）。
    let _ = id;
}
