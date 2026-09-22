# trace（os/core/src/trace/）

> **结构化事件记录**：`seq` 是唯一的定序 / 断言依据，`timestamp` 只是元数据；固定容量 ring，热路径 O(1)、无分配。
> 是 CoreTest 断言与未来确定性重放的证据基础（见 `docs/development/testing.md` §4）。

## owns 什么真相

- 事件序号 `seq`（单调，耗尽后停止记录、绝不回绕）。
- 每个事件 kind 的运行时使能掩码。
- 固定容量 ring 的记录 + 统计（capacity / oldest_seq / next_seq / overwritten_total / enabled_mask）。
- ABI 编码：`TraceRecordAbi`（48 字节）、`TraceStatsAbi`（40 字节）、`KIND_*`。

## 暴露什么机制

- `emit(TraceEvent)`（热路径；`CONFIG_TRACE=n` 时内联空操作）。
- 读侧：`visit_since(seq, visitor)`（有界实时遍历，非原子快照）、`stats()`、`clear()`、`capacity()`、`next_seq()`、`enabled_mask()`。
- Core 管理侧：`set_enabled_mask()`（Monitor `trace` 命令驱动；组件无此权限）。
- 类型：`TraceStats`、`TraceRecord`、`TraceEvent`、`RejectReason`、`ENABLED_MASK_ALL`。

## 明确不做

- 不做动态订阅 / 过滤引擎 / 落盘 / 用户态守护进程。
- 组件**不能**设置全局掩码，只能经 `kcore_trace_stats` 读。
- 尚未定义 `TaskBlock` / `TaskWake` / `Fault` 事件（Core 还没有对应 chokepoint，有再加）。
- `clear()` 只清记录与逐出计数、不回绕 `seq`；"序号回到 1"的完整复位是 test-only（`reset_for_test`）。
- host 测试的 ring 是线程本地替身，**不覆盖生产的锁 / 并发语义**（不能当 SMP 覆盖读）。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/trace/mod.rs` | 模块入口、emit / visit / stats 门面 |
| `os/core/src/trace/event.rs` | `TraceEvent`、`RejectReason`、`TraceRecord` |
| `os/core/src/trace/ring.rs` | ring 缓冲、掩码、统计、emit / visit / stats |
| `os/core/src/trace/abi.rs` | `TraceRecordAbi` / `TraceStatsAbi` 编码、`KIND_*` |
| `os/core/src/trace/abi/tests.rs` / `os/core/src/trace/ring/tests.rs` | 编码 / ring 语义测试 |
