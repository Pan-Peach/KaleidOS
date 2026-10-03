# monitor（os/core/src/monitor/）

> **Core Monitor 交互 shell**（`core>`）：裸 Core 常态能力，不依赖任何组件，启动后即挂上。
> 它是调试 / 观察面，**不持有权威状态**；唯一副作用是 `mount` 时提交 `MachineInfo`。

## owns 什么真相

无。它是命令循环 + 命令实现 + 纯行编辑器。
`cmds::mount(info)` 会调用 `crate::machine::commit(*info)`——这是 `MachineInfo` 的提交点。

## 暴露什么机制

- `run() -> !`：主循环。
- 读串口前服务本 CPU 的 Runnable 组件任务，避免 ksh 空闲 yield 回锚点时抢读命令；没有调度策略时继续接受 monitor 输入。
- `mount(info)`：提交机器信息并进入 shell。
- 命令（`cmds.rs` 中各自 `pub fn`）：`help`、`machine`、`memory`、`tasks`、`load`、`unload`、`components`、`catalog`、`trace`、`shutdown`、`reboot`。
- `editor` 模块：`LineEditor`、`Outcome`、`Screen`、`VecScreen`——纯状态机（无 I/O、无堆分配），支持光标移动 / 退格 / Ctrl-U·K·W / 历史 / Tab 补全。

## 明确不做

- **无 god-mode 变更**：查询只读；不会因为"它拥有 console"就获得权限。
- 组件没有全局 trace-control 权限：掩码控制是 Core 管理路径（monitor `trace` 命令），组件只能经 `kcore_trace_stats` 读。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/monitor/mod.rs` | 主循环 + 命令表 + `run` / `mount` |
| `os/core/src/monitor/cmds.rs` | 各命令实现（`mount` 提交 `MachineInfo`） |
| `os/core/src/monitor/editor.rs` | 纯行编辑器状态机 |
