# build_config（os/core/src/build_config.rs）

> **`build.rs` ↔ Kconfig 的窄传输契约**：只负责 `TRACE_CAPACITY` 的解析与再校验。
> **仅在 `#[cfg(test)]` 下编译**（`lib.rs` 声明），通过 `#[path]` 与 `os/core/build.rs` 共享同一份逻辑。

## owns 什么真相

- `TRACE_CAPACITY` 从环境变量（Makefile 转发 `CONFIG_TRACE_CAPACITY`）到 OUT_DIR 常量的解析 / 校验契约。
- 常量：`TRACE_CAPACITY_DEFAULT`、`TRACE_CAPACITY_MIN`、`TRACE_CAPACITY_MAX`。

## 暴露什么机制

- `parse_trace_capacity(raw, required) -> Result<u32, TraceCapacityError>`。
- `TraceCapacityError`。

## 明确不做

- **不读 `.config`**：Kconfig 仍是唯一真相；这里的 range 只是**防御性再校验**，不重新解释 Kconfig 的默认值 / 范围。
- 坏值**不会**静默退回默认：裸机构建缺值 / 越界直接报错；host 构建用显式默认。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/build_config.rs` | `parse_trace_capacity`、错误类型、上下界常量 |
| `os/core/build.rs` | 同源逻辑的生产侧消费者（经 `#[path]` 复用） |
