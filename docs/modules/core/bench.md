# bench（os/core/src/bench/）

> **benchmark harness**：批量计时、时钟刻画、K 校准、报告，以及从 trace 抽取 IRQ 延迟三元组。
> 与正确性测试**严格分离**：正确性看断言，性能看趋势（见 `docs/development/benchmark.md`）。

## owns 什么真相

- 测量方法学：batch 总时长统计（不是单次操作 p95）、有界时钟探测、K 校准 policy、配对 null baseline。
- 报告格式与状态：`BENCH-ENV` / `BENCH <name>` / `key=value`，`status`（`ok` / `clock_unusable` / ...）。
- 从 trace 抽取 `IrqEnter -> IrqDispatch -> IrqAck` 三元组（software-instrumented 区间）。

## 暴露什么机制

- `Bench`、`BenchResult`、`BenchStatus`、`ClockUnit`。
- `now()` / `clock_unit()` / `clock_source()` / `run()` / `report_environment()`。
- 常量：`ROUNDS`、`BATCHES_PER_ROUND`、`SAMPLE_CAP`、`MAX_OPS_PER_BATCH`。
- 重导出 batch policy（`ClockProbe`、`MeasurementPlan`、`probe_clock`、`choose_ops_per_batch`、`summarize` 等）与 IRQ（`IrqLatency`、`collect_irq_latency`、`irq_latency`）。

## 明确不做

- **不做平台 / QEMU 探测**：Core 没有运行时板级探测；`platform` 如实写 `undetected`，QEMU 与真机的区分由 runner 记录。
- **不造综合总分**；不在测量循环里打印。
- host / 目标端数字不可直接比较（单位不同：ns vs `timebase-ticks`）。
- `collect_irq_latency` 抽的是 claim 后→complete 前的**插桩软件区间**，不是"中断投递延迟"。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/bench/mod.rs` | harness 主体 + clock 模块 |
| `os/core/src/bench/report.rs` | `BenchResult`、`BenchStatus`、`report_environment` |
| `os/core/src/bench/irq.rs` | 从 trace 抽取 IRQ 延迟 |
| `os/core/src/bench/batch/mod.rs` | 批量计时 policy / 校准 |
| `os/core/src/bench/batch/tests.rs` / `os/core/src/bench/tests.rs` | host 测试 |
