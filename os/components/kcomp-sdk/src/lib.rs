//! kcomp-sdk —— 组件 SDK / CRT（step 2）。
//!
//! 这个 crate 解决三件互相独立的事，全部**随 `.kcomp` 私有携带**（不建 shared
//! Rust runtime，见 docs/architecture/component-model.md §2.2）：
//!
//! 1. [`abi`]：`kcore_*` 导出白名单的**单一来源**（组件不再各自复制 extern 块）；
//! 2. 入口 / 日志 / panic adapter：`kcomp_instance_create!` /
//!    `kcomp_instance_destroy!`、[`log`]/`klog!`、`#[panic_handler]`；
//! 3. 可选的 alloc adapter（feature `alloc`）：`GlobalAlloc` → Core 共享堆。
//!
//! 模块划分与 crate 外部路径一一对应（`abi` / `DmaDirection` / `log` /
//! `console_write_byte` 保持原路径不变）：
//! [`abi`] 原始 extern + 共享 ABI 值类型（`InterfaceAbi` / `InterfaceKind`）、
//! [`endpoint`] typed `Endpoint<C>`（Contract / Endpoint 模型）、
//! [`frame`] flat frame 的借用视图、
//! [`block`] block.device 契约 + provider wrapper + **调用后端**（Core 在 bind 时
//! 选定的 Direct / Gate，`BlockBinding` typed 前端）+ Gate 适配器、
//! [`scheduler`] `scheduler.policy` 契约（Gate-only；consumer = Core）、
//! [`call`] endpoint call 的原始包装、`dma`、`logging`、`panic`、`alloc`。
//!
//! [`kcomp_services!`] 生成 image 级 port switch（`kcomp_service_dispatch`）；
//! method switch 由契约自己的适配器（如 [`block::dispatch`]）手写。
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
pub mod block;
pub mod call;
pub mod endpoint;
pub mod errno;
pub mod filesystem;
pub mod frame;
pub mod generated;
pub mod probe;
pub mod scheduler;

mod dma;
mod logging;

#[cfg(test)]
mod test_support;

// 裸机专属 adapter：host 构建下 std 自带 panic handler / 分配器。
#[cfg(all(target_os = "none", feature = "alloc"))]
mod alloc;
#[cfg(target_os = "none")]
mod panic;

pub use dma::DmaDirection;
pub use errno::{Errno, Result};
pub use logging::{console_write_byte, log};

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// 组件生命周期入口约定（docs/architecture/component-lifecycle.md §4）
// ---------------------------------------------------------------------------

/// 定义组件实例创建入口 `kcomp_instance_create`，并发出契约指纹 `kcomp_abi`。
///
/// 用法：`kcomp_instance_create!(|args, out_state| { ...; 0 })`。参数标识符由
/// **调用点**给出（closure 风格），因此 body 可以直接命名它们——这正是
/// macro_rules 卫生性需要的：`*out_state = state;` 在 body 里可见。
///
/// 生成 `extern "C" fn kcomp_instance_create(args: *const KcompCreateArgs,
/// out_state: *mut *mut ()) -> i32`，外加 `#[unsafe(no_mangle)] pub static
/// kcomp_abi: u64`（值 = [`abi::KCOMP_ABI`]）与 `abi::KcompInstanceCreate`
/// 编译期锚定。宏内建 `#[allow(unused_variables)]`（`|_args, _out_state|` 不告警）
/// 与 `#[allow(clippy::not_unsafe_ptr_arg_deref)]`（写回 `*out_state` 是 ABI 契约，
/// Core 保证可写）。
///
/// 返回 `0` / `-errno`（旧的"非零 = 失败 bitmap"约定已废弃）。Core 调用前把
/// `*out_state` 初始化为 `NULL`；成功时组件写入自己完成的 state 指针，
/// **无状态组件可以不写**（保持 NULL）。create 失败 / panic → 走 Core 的
/// Failed 路径，且**不会**调用 destroy（构造期清理由组件自己负责）。
///
/// 一个组件只写一次本宏，析构用 [`kcomp_instance_destroy!`]。
#[macro_export]
macro_rules! kcomp_instance_create {
    (|$args:ident, $out_state:ident| $body:block) => {
        #[unsafe(no_mangle)]
        #[allow(unused_variables)]
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn kcomp_instance_create(
            $args: *const $crate::abi::KcompCreateArgs,
            $out_state: *mut *mut (),
        ) -> i32 $body

        // 精确契约指纹（手工维护，非版本号）；Core 调用组件前校验。
        #[unsafe(no_mangle)]
        pub static kcomp_abi: u64 = $crate::abi::KCOMP_ABI;

        // 编译期锚定：生成的函数必须与 `abi` 的 Rust 镜像同签名。
        const _: $crate::abi::KcompInstanceCreate = kcomp_instance_create;
    };
}

/// 定义组件实例析构入口 `kcomp_instance_destroy`。
///
/// 用法：`kcomp_instance_destroy!(|state| { ...; 0 })`。参数标识符由**调用点**
/// 给出（同 create 的 closure 风格），body 可以直接命名它。
///
/// 生成 `extern "C" fn kcomp_instance_destroy(state: *mut ()) -> i32` 与
/// `abi::KcompInstanceDestroy` 编译期锚定；宏内建 `#[allow(unused_variables)]`
/// 与 `#[allow(clippy::not_unsafe_ptr_arg_deref)]`。
///
/// 返回 `0` / `-errno`；destroy 失败 / panic → Core 把实例置 Failed 并保留
/// 内存（**绝不自动重试**）。Core 对未完整构造 / panic 的实例不调用本入口。
///
/// destroy 只做组件自己的 quiesce / 私有资源清理；Core 仍会兜底
/// revoke authority / 失效 endpoint。已交给外部（`'static` SDK 引用）的 state 存储
/// 本轮保留——consumer 可能持有拷贝过的 typed 前端句柄。
#[macro_export]
macro_rules! kcomp_instance_destroy {
    (|$state:ident| $body:block) => {
        #[unsafe(no_mangle)]
        #[allow(unused_variables)]
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn kcomp_instance_destroy($state: *mut ()) -> i32 $body

        // 编译期锚定：生成的函数必须与 `abi` 的 Rust 镜像同签名。
        const _: $crate::abi::KcompInstanceDestroy = kcomp_instance_destroy;
    };
}

// ---------------------------------------------------------------------------
// Endpoint 服务入口：image 级 port switch
// ---------------------------------------------------------------------------

/// 定义组件的 **image 级服务入口** `kcomp_service_dispatch`（可选导出；Core 的
/// `kcore_endpoint_call` 经它分派），并发出与生命周期宏同类的签名锚定。
///
/// 用法：
///
/// ```text
/// kcomp_sdk::kcomp_services! {
///     state: DeviceState;
///     BLOCK_PORT => block::dispatch::<DeviceState>,
/// }
/// ```
///
/// 展开成 `match port { BLOCK_PORT => handler(state, method, call), _ => ENOSYS }`。
///
/// - **只做 port switch**：契约自己的 method switch 由 handler 手写（block 用
///   `block::dispatch`）；宏不发明协议、不解析 frame。
/// - **不发布 endpoint**：发布名 / 时机来自 create config 与组合策略，publication
///   是 staged 的；本宏只看 provider 定义的 `port` token。
/// - **端口 pattern 重复 = 编译错误**（逐对 `const` 断言，不静默覆盖）。
/// - handler 收到 provider state 的 `&State`（**不是** `&mut`）：实例可能被多个
///   执行流同时引用（任务 / 调用），可变性由组件自己用内部同步管理。
/// - `instance_state` 为 NULL → `-EINVAL`（本宏面向有状态 provider；不构造 UB 引用）。
#[macro_export]
macro_rules! kcomp_services {
    (state: $state:ty; $($port:path => $handler:expr),+ $(,)?) => {
        #[unsafe(no_mangle)]
        #[allow(unused_variables)]
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn kcomp_service_dispatch(
            instance_state: *mut (),
            port: u32,
            method: u32,
            frame: *const $crate::abi::KcompCallFrame,
        ) -> i32 {
            if instance_state.is_null() {
                return $crate::errno::Errno::EINVAL.code();
            }
            // SAFETY: Core 的 service 边界保证 instance_state 是 create 写回的实例
            // state 且在本调用期间有效；非空已在上方检查。
            let state = unsafe { &*instance_state.cast::<$state>() };
            // SAFETY: Core 的 service 边界保证 frame 有效、其内存区在本调用期间可用。
            match unsafe {
                $crate::frame::with_call(frame, |call| match port {
                    $($port => ($handler)(state, method, call),)+
                    _ => $crate::errno::Errno::ENOSYS.code(),
                })
            } {
                Ok(status) => status,
                Err(errno) => errno.code(),
            }
        }

        // 编译期锚定：生成的函数必须与 `abi` 的 Rust 镜像同签名。
        const _: $crate::abi::KcompServiceDispatch = kcomp_service_dispatch;

        // 端口 token 唯一：重复即编译错误（不静默覆盖 / 不取最后一个）。
        $crate::__kcomp_services_ports_unique!($($port),+);
    };
}

/// [`kcomp_services!`] 的内部辅助：逐对比较端口 pattern（`const` 求值）。
/// `#[doc(hidden)]`；不要直接调用。
#[doc(hidden)]
#[macro_export]
macro_rules! __kcomp_services_ports_unique {
    ($($ports:expr),+ $(,)?) => {
        $crate::__kcomp_services_ports_unique!(@pairs [] $($ports),+);
    };
    (@pairs [$($seen:expr)*] $head:expr $(, $tail:expr)*) => {
        $(
            const _: () = assert!(
                $seen != $head,
                "kcomp_services!: duplicate port pattern（端口 token 必须在组件内唯一）"
            );
        )*
        $crate::__kcomp_services_ports_unique!(@pairs [$($seen)* $head] $($tail),*);
    };
    (@pairs [$($seen:expr)*]) => {};
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
