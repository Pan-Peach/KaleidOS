//! kcomp_smoke —— 第一个 .kcomp 组件。
//!
//! 目的：验证组件加载链路（打包 → 传输 → 放段 → 调 init），不做任何事。
//!
//! 约束（对应 AGENTS.md 的 Authority ≠ Interface）：
//! - no_std：组件运行在内核地址空间（KernelNative 执行域），没有宿主 std；
//! - 不自带 panic handler：组件是可变对象，panic=abort 语义由加载方（内核）统一决定；
//!   （编译成 .o 不链接，rustc 不会检查 panic handler —— 链到内核时由内核全局提供）
//! - `kcomp_init` 是约定入口（Linux .ko 的 module_init 简化版），
//!   用 `#[no_mangle]` 保证符号名 `kcomp_init` 出现在 ELF 符号表里（insmod 帮我们找到的符号）。

#![no_std]

/// 组件入口：加载器（内核侧）在放段 + 重定位之后调用。
///
/// 返回约定（Linux insmod 风格）：0 = 加载成功；非 0 = 加载失败（错误码留给未来扩展）。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_init() -> i32 {
    0
}
