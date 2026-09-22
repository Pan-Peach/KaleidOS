//! 组件面 `Errno` 门面：类型本体由 `tools/kabi/kabi_gen.py` 从 `abi/errno.toml`
//! 生成到 [`crate::generated::errno`]；本文件只保留手写部分。
//!
//! `no_std` 下**没有**标准库 errno 类型可复用——现成的 crate（`libc` / `errno` /
//! `rustix` / `linux-errnos` / `tiny-std` …）全部门控在 hosted / Linux target，
//! 没有一个支持 `riscv64gc-unknown-none-elf`。但**编号本身就是标准**：
//! `abi/errno.toml` 持有完整的 Linux/POSIX `asm-generic/errno` 集合（1–133），
//! 组件不依赖 `os/core`（那会把 Core 的 Rust 类型带进 `.kcomp`），而是消费同一
//! schema 生成的镜像。
//!
//! # ABI 约定
//!
//! ```text
//! 0          success
//! -negative  failure: -Errno
//! ```
//!
//! 组件调 `kcore_*`（裸 `i32`）后用 [`Errno::from_code`] 解码；SDK 的人体工学层
//! （[`crate::binding`] / [`crate::block`]）直接用 [`Result`]。

pub use crate::generated::errno::Errno;

/// `kcore_*` 返回的裸 `i32`（`0` / `-errno`）解码成的结果类型。
pub type Result<T> = core::result::Result<T, Errno>;

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}
