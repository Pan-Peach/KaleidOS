# timer（os/core/src/timer/mod.rs）

> **timer 机制状态**：是否已初始化、tick 计数、下一个 deadline，以及抢占式时钟的初始化。
> 具体时钟硬件（SBI TIME / CLINT）在 `arch`。

## owns 什么真相

- timer 机制状态：**`delivery_ready`**（投递链路端到端就绪的单一 readiness 标志）、tick 计数、next deadline。
- 抢占模式的初始化开关（`init_preempt(timebase_hz)`）。

## 暴露什么机制

- `init()`、`init_preempt(timebase_hz)`。
- `arm_deadline(deadline)`：**关中断**下编程一次性 deadline；只有硬件接受才发布新的 `next_deadline`。
- `delivery_ready()`：本 CPU 是否已有可用投递（不分配，早期 boot / 轮询回退可安全查询）。
- `on_trap()`：trap 侧驱动 tick / 过期。
- `ticks()`。
- `TimerError`（含 `Backend(arch::TimerError)`，保留后端原始失败原因）。

## 明确不做

- **不认识 SBI / CLINT / PIT / CNTV 细节**：走 `arch::TimerImpl` backend。
- 在协作式 profile 下**不自动产生周期 tick**；`init` 失败时 `core::init` 可继续（idle 回退轮询），只有需要 timer 驱动的 `preempt` profile 才 fail-closed。
- 无 per-component `TimerHandle`。
- 抢占尚未完成：`sched::on_timer_tick` 仍是 `todo!`（C5 未落地，见 `STATUS.md` 3.6）。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/timer/mod.rs` | timer 状态、`arm_deadline`、preempt 初始化 |
