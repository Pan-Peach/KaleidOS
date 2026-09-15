//! kcomp-sdk —— 组件 SDK / CRT（step 2）。
//!
//! 这个 crate 解决三件互相独立的事，全部**随 `.kcomp` 私有携带**（不建 shared
//! Rust runtime，见 docs/component-model.md §2.2）：
//!
//! 1. [`abi`]：`kcore_*` 导出白名单的**单一来源**（组件不再各自复制 extern 块）；
//! 2. 入口 / 日志 / panic adapter：`kcomp_init!`、[`log`]/`klog!`、`#[panic_handler]`；
//! 3. 可选的 alloc adapter（feature `alloc`）：`GlobalAlloc` → Core 共享堆。
//!
//! 模块划分与 crate 外部路径一一对应（`abi` / `binding` / `DmaDirection` / `log`
//! / `console_write_byte` 保持原路径不变）：
//! [`abi`] 原始 extern、[`binding`] 类型化 Service 契约、[`block`] block.device
//! 契约 + provider wrapper、`dma`、`logging`、`panic`、`alloc`。
//!
//! # panic adapter（本 crate 存在的关键理由）
//!
//! 组件以链接后的 ET_REL 加载，镜像里带自己的 `#[panic_handler]`——组件 `panic!`
//! 时进入的是这里，而不是 boot 镜像的 panic handler。adapter 只做两件事：
//! 经 [`abi::kcore_log_line`] 打印一行诊断，然后调 [`abi::kcore_panic_escape`]
//! 把控制权交还 Core（活动 containment 边界内它**永不返回**，见
//! `component/containment.rs`）。若没有活动边界（返回 `-EPERM`），说明这次 panic
//! 不在任何组件边界内，只能停在原地自旋（安全失败）。
//!
//! # alloc adapter
//!
//! `#[global_allocator]` 不是"每个组件自带堆"：它只是把 Rust `GlobalAlloc`
//! 契约接到 **Core 共享堆**（`kcore_heap_alloc/dealloc`）。默认关闭，组件按需
//! 通过 `kcomp-sdk = { path = "...", features = ["alloc"] }` 开启。

#![no_std]

// host 测试用（`cargo test`）；裸机目标不编入。
#[cfg(test)]
extern crate std;

pub mod abi;
pub mod binding;
pub mod block;

mod dma;
mod logging;

// 裸机专属 adapter：host 构建下 std 自带 panic handler / 分配器。
#[cfg(all(target_os = "none", feature = "alloc"))]
mod alloc;
#[cfg(target_os = "none")]
mod panic;

pub use dma::DmaDirection;
pub use logging::{console_write_byte, log};

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// 组件入口约定
// ---------------------------------------------------------------------------

/// 定义加载入口 `kcomp_init`（Linux module_init 风格）。
///
/// 用法：`kcomp_sdk::kcomp_init!({ ...; 0 })`。块的值即返回码：`0` = 成功，
/// 非 0 = 失败位图（Core 据此标记 Failed）。与手写
/// `#[unsafe(no_mangle)] pub extern "C" fn kcomp_init() -> i32` 完全等价。
#[macro_export]
macro_rules! kcomp_init {
    ($($body:tt)*) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn kcomp_init() -> i32 {
            $($body)*
        }
    };
}

// 可选退出入口 `kcomp_exit`（Linux module_exit 风格）。
//
// 组件用与 `kcomp_init!` 对称的宏声明退出钩子；没有收尾工作的组件写**显式
// no-op**，让"没有退出逻辑"本身也是一行声明（kcomp_smoke 是带证据行的参考）。
//
//     kcomp_sdk::kcomp_exit!(0);
//
// Core loader 会可选地解析该符号（`LoadedComponent::exit` /
// `ComponentRecord::exit`）；monitor `unload` 驱动的停止路径
// （`os/core/src/component/exit.rs::stop_component`）在实例 `Ready` 时于
// Core-owned 隔离栈上调用它：`Ready → Stopping → Stopped`。
// **loader 不要求该符号**：不导出 = 该组件没有退出钩子（`exit == None`），
// 停止时跳过钩子，这是正常情况。
//
// 返回码约定（**暂定**，与 `kcomp_init` 对称）：`0` = 干净退出；非 0 / panic
// 目前镜像 init 失败语义（`Failed` + Core 兜底 revoke）。最终语义待人类定稿，
// 见 `docs/component-model.md` §5.2。
#[macro_export]
macro_rules! kcomp_exit {
    ($($body:tt)*) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn kcomp_exit() -> i32 {
            $($body)*
        }
    };
}

// ---------------------------------------------------------------------------
// 日志宏
// ---------------------------------------------------------------------------

/// 组件日志宏：`klog!("state={}", value)` → [`log`]。
#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => {
        $crate::log(::core::format_args!($($arg)*))
    };
}
