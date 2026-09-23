# errno（os/core/src/errno.rs）

> **内部错误 → ABI `Errno` 的唯一翻译点**。
> ABI 约定：`0` = 成功，`-negative` = 失败（`-Errno`）。数值照抄 Linux/POSIX，不发明。

## owns 什么真相

- 稳定 ABI 错误命名空间（`Errno` 定义在生成的 `generated::errno`，1–133）。
- 每个内部错误类型到 `Errno` 的**完整映射表**——只有一处，别处不再各自翻译。

## 暴露什么机制

- `pub use crate::generated::errno::Errno`。
- `Errno::code(self) -> i32`（= `-(self as i32)`）。
- `pub(crate) fn status<E: Into<Errno>>(Result<(), E>) -> i32`。
- `From<...> for Errno` 覆盖：`TaskError`、`SchedError`、`EndpointError`、`CallError`、`ComponentLoadError`、`ComponentStopError`、`machine::DeviceLookupError`、`DeviceClaimError`、`DeviceReleaseError`、`DmaError`、`IrqError`。
- host 测试用穷尽 match 把每个映射钉到一个数字：新增枚举变体会直接编译失败（防漏映射）。

## 明确不做

- **不统一内部错误**：`TaskError` / `SchedError` / `DeviceClaimError` … 保持丰富且类型安全，只在 ABI 边界翻译。
- 组件侧不手写 `const E*`：Rust 用 `kcomp_sdk::Errno` / `Result<T>`，C 用 SDK 的 `<errno.h>` shim；线格式仍是裸 `i32`。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/errno.rs` | `Errno` re-export、`code` / `status`、各内部错误的 `From` 映射 + 穷尽测试 |
| `os/core/src/generated/errno.rs` | 生成的 `Errno` 枚举（见 [`generated.md`](generated.md)） |
