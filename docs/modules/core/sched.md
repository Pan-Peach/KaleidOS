# sched（os/core/src/sched.rs）

> Core 的调度提交与上下文切换机制。职责契约见 `docs/architecture/scheduling.md`。

## owns 什么真相

- 每 CPU 的 current task、锚点上下文及 incoming / anchor IRQ 状态。
- 当前 policy EndpointId、调用栈、占用与退役状态。
- 任务状态与固定 CPU 归属保存在 task table，由 commit 复验并推进。

## 暴露什么机制

- `set_policy` / `select_provider`：验证并显式选择 scheduler.policy。
- `run` / `yield_current` / `park_current` / `exit_current`：候选快照 → 组件提议 → Core 原子提交 → 切换。
- `unpark_task`：owner 验证、permit / wake 提交，通知目标 CPU。
- `request_reschedule`：本地 pending 或远端 IPI；AP 空闲循环和 BSP 空闲安全点服务工作。
- policy 回调串行占用同一张 Core 栈；候选失效重试，错误提议或 panic 退役 provider。
- IRQ 关闭至 incoming 栈与边界安装完成，恢复 incoming 保存值；没有锁或 IRQ RAII guard 跨切换持有。

## 明确不做

- RR 游标、优先级、公平性、私有 runqueue 属于 scheduler 组件。
- 当前不迁移任务、不 work steal、不抢占；`on_timer_tick` 仍未实现。
- 不定义等待条件、事件对象或组件等待队列。
- 私有 AS 任务调度未实现。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/sched.rs` | 配置、快照、策略调用、提交与切换 |
| `os/core/src/task/table.rs` | commit 事务、permit、任务状态真相 |
| `os/core/src/component/containment.rs` | 每 CPU 身份与 panic / policy 边界 |
| `os/core/src/smp/mod.rs`、`ipi.rs` | AP 调度循环、Online、Reschedule 门铃 |
| `os/components/scheduler_rr/src/lib.rs` | 每 CPU 的 RR 算法状态 |
| `os/components/tests/core_test/src/runtime/smp.rs` | 公开 ABI 上的 SMP 组件调度集成编排 |
| `os/components/tests/kcomp_smp/` | 独立的任务 panic 被测对象 |
