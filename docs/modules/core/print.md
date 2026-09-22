# print（os/core/src/print.rs）

> **核心日志格式化**：格式化在 Core，**传输**交给 `arch` 的 `Console` backend（host = Fake/std，RISC-V = OpenSBI）。

## owns 什么真相

无资源真相。它只负责把 `printk!` / `log!` 的参数格式化成字节并交给 Console backend，以及交互式读取一行 / 空闲等待。

## 暴露什么机制

- `print()`、`log(tag, args)`、`print_bytes()`。
- `read_line()`：monitor 行编辑读取。
- `idle_wait()`（含内部 `idle_period`、`ConsoleScreen`）：空闲不忙等，arm 一次性 timer 后 `wfi`。
- 宏：`printk!`、`log!`（`#[macro_export]`）。

## 明确不做

- **不处理 panic 路径**：panic 走 boot 的静态应急 console（直接 SBI 打印诊断）。
- 不直接依赖 SBI / UART / std：传输经 `arch::ConsoleImpl`，与具体 backend 解耦。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/print.rs` | 格式化、Console 传输、`read_line` / `idle_wait`、`printk!` / `log!` |
