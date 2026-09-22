# sched（os/core/src/sched.rs）

> **每 CPU 调度真相 + propose→validate→commit 路径**：Core 提供机制，策略组件只"提议"下一个任务；Core 验证存在 / Runnable / 未在别 CPU 后提交。
> 这是 "Policy proposes, Core validates and commits" 的规范实现。

## owns 什么真相

- 每 CPU 调度状态：`CpuState { anchor, current }`（当前任务锚点）。
- 从"提议"到"提交"的最终裁决：验证 task 存在 / Runnable / 不在别的 CPU；提交后记录 trace。
- `SCHEDULER_POLICY_ABI`：SchedulerPolicy 的 exact ABI fingerprint（组件替换 = 换 provider 实现同一 layout）。

## 暴露什么机制

- `SchedulerPolicyApi`：`#[repr(C)]` function table（组件提供的策略 ABI）。
- `SCHEDULER_POLICY_ABI`；`SchedError`。
- `init()`、`current_task()`。
- `run()`：从锚点进入调度循环。
- `yield_current()` / `exit_current()`。
- `on_timer_tick()`（`todo!`，抢占未落地）、`abort_current_task()`。

## 明确不做

- **不实现任何调度算法**：RR / CFS 在 `scheduler_rr` 等组件里。
- **没有内建 / 兜底调度器**：没有绑定 policy 时返回 `NoPolicy`，不猜测、不静默降级。
- 拒绝在 IRQ 上下文 `run` / `yield` / `exit`。
- 不持有任务 handle：策略只收候选 `TaskId` 并提议。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/sched.rs` | `SchedulerPolicyApi`、`CpuState`、`run` / `yield_current` / `exit_current`、验证与提交 |
