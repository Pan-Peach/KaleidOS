//! `kcore_*` 导出 ABI（EXPORT_SYMBOL 教学版）+ 组件生命周期入口的 Rust 镜像。
//!
//! **本文件是手写 facade**：声明本体（结构、常量、fn 指针类型、extern 块、
//! 生命周期入口别名、接口分类枚举）由 `tools/kabi/kabi_gen.py` 从
//! `abi/component.toml` + `abi/core.toml` 生成到 [`crate::generated::abi`]，
//! 这里只做 re-export 并保留模块级说明。**改 ABI = 改 abi/*.toml**，然后
//! `make abi-gen`；C 侧作者面是 `include/kcomp.h`（umbrella）+
//! `include/generated/kcomp_abi.h`（`AGENTS.md`：Rust ABI 永不成为组件 ABI）。
//!
//! 声明即契约：名字必须与 Core `component/export.rs` 的白名单逐字节一致，签名
//! 错误 = UB（loader 只按名字精确解析，不校验签名）。Core 侧的导出注册表
//! （`os/core/src/component/generated/exports.rs`）用 typed 引用锚定实现签名，
//! 生成物里的 `_Static_assert` / `const _` 锚定布局。宽度规则：Rust `usize`
//! ↔ C `size_t`（指针宽）；counts/ids → `u32`；不透明句柄 → `u64`。
//!
//! # Trace 支持状态（编译期 vs 运行时，组件要能分开发现）
//!
//! - **编译期**：Core 以 `CONFIG_TRACE=n` 构建时 `trace::emit` 是内联空操作，
//!   ring 恒为空 —— `kcore_trace_read` 永远 `-ENOENT`，
//!   `kcore_trace_stats` 的 `enabled_mask == 0`。这是"这台机器没带 trace"，
//!   不是"事件被过滤"。
//! - **运行时**：`enabled_mask` 报告哪些事件 kind 会被记录（bit i ↔ kind i+1，
//!   即生成物里的 `KIND_*` 常量；12 位掩码，默认全开 = `0x0fff`，高位保留恒 0）。
//!   被过滤的事件不记录、**不消耗 `seq`**。掩码由 Core 管理路径（Monitor）
//!   控制：组件只能**读**（`kcore_trace_stats`），没有写入口。
//!
//! 读侧是**有界实时遍历，不是原子快照**：读取之间发生覆盖时，`since` 落在已
//! 逐出区间，`kcore_trace_read` 返回当前最旧存活记录 —— reader 的真实缺口 =
//! `record.seq - since`（`TraceStatsAbi::overwritten_total` 只表示 ring 因满
//! 逐出了多少条，不等于某个 reader 漏掉的条数）。

pub use crate::generated::abi::*;
