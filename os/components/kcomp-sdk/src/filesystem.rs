//! `filesystem` 契约（KIND = Service）+ provider 侧 ergonomic wrapper。
//!
//! 本模块是 filesystem 契约的**语义**住处：契约类型（[`FileSystem`]）与 provider
//! 包装（[`FileSystemProvider`] / [`FileSystemService`]）放在一起；
//! `#[repr(C)]` function table（[`FileSystemApi`]）与名字 / 指纹 / contract 身份 /
//! 扁平方法常量由 `tools/kabi/kabi_gen.py` 从 `abi/filesystem.toml` 生成
//! （[`crate::generated::filesystem`]），这里 re-export；[`crate::binding`] 只
//! re-export，保持 `binding::FileSystem` 等既有路径不变。
//!
//! # 同一 ABI 指纹，两种 transport（与 block.device 同构）
//!
//! - **Direct**：[`FileSystemApi`]（`#[repr(C)]` function table）+ opaque `ctx`——
//!   endpoint 发布时作为 `api` / `ctx` 交付；同域调用方直接调 table（稳态零 Core
//!   介入、零分配、零打包）。provider 侧适配器见 [`FileSystemService`]。
//! - **Gate** ：[`dispatch`] 的扁平方法编码（schema = `abi/filesystem.toml`）——
//!   跨域 / 受控绑定经 `kcore_endpoint_call` 调用 provider 的
//!   `kcomp_service_dispatch`；业务后端与 Direct 是**同一份** provider 实现。
//!
//! **机制由 Core 在 bind 时按两端执行域选定**；provider 两种都提供、不选择
//! （`docs/architecture/deployment.md` §2）。
//!
//! ```text
//! impl FileSystemProvider for MyFs { ... }        // provider：纯 Rust，无 unsafe
//! static FS: FileSystemService<MyFs> = FileSystemService::new(MyFs { ... });
//! FS.publish_endpoint(FILESYSTEM_NAME, FS_PORT)?;  // endpoint 模型（Direct + Gate）
//! ```
//!
//! consumer 侧见 [`client::FileSystemBinding`]（Core 在 bind 时选定机制）。

use core::ffi::CStr;

use crate::binding::{InterfaceAbi, InterfaceKind, Service};
use crate::endpoint::Contract;
use crate::errno::Result;

mod backend;
pub mod client;
pub mod dispatch;
mod service;

#[cfg(test)]
pub(crate) mod tests_support;

// -----------------------------------------------------------------------
// 契约：filesystem —— 只读文件系统服务（provider: fatfs 等；consumer: 未来的 VFS）
// -----------------------------------------------------------------------

/// filesystem 服务的稳定 endpoint 名字（publish / bind 必须逐字节一致）。
pub use crate::generated::filesystem::KCOMP_FILESYSTEM_NAME as FILESYSTEM_NAME;

use crate::generated::filesystem::{KCOMP_FILESYSTEM_ABI, KCOMP_FILESYSTEM_CONTRACT};

/// filesystem 契约的 exact ABI fingerprint。
///
/// 数值 = 8 字节 ASCII tag `b"FILESYST"` 的大端读数
/// （`0x4649_4C45_5359_5354`，可直接按字节读出拼写）。**同一个契约只此一个指纹**：
/// 新增 transport（Gate 的扁平编码）不改变它——ABI 标识的是 function table /
/// 扁平编码的逐位布局，transport 的选择是 Core 在 bind 时的机制决定。
///
/// raw `u64` 本体在生成物（[`KCOMP_FILESYSTEM_ABI`]，schema 单一来源）；
/// [`InterfaceAbi`] newtype 由手写 `binding.rs` 定义，这里做包装。
pub const FILESYSTEM_ABI: InterfaceAbi = InterfaceAbi::from_raw(KCOMP_FILESYSTEM_ABI);

/// 第一阶段只读文件访问。flags 是 ABI 编码，不直接暴露 FatFs 的 `FA_*`。
pub use crate::generated::filesystem::KCOMP_FILESYSTEM_OPEN_READ as FILESYSTEM_OPEN_READ;

/// `filesystem` provider/consumer function table（**Direct** transport）。
///
/// 定义与布局断言在生成物 [`crate::generated::filesystem`]（schema =
/// `abi/filesystem.toml`）；这里 re-export 保持既有路径。
pub use crate::generated::filesystem::FileSystemApi;

/// filesystem 契约（KIND = Service）。
pub struct FileSystem;

impl Service for FileSystem {
    const NAME: &'static [u8] = FILESYSTEM_NAME;
    const KIND: InterfaceKind = InterfaceKind::Service;
    const ABI: InterfaceAbi = FILESYSTEM_ABI;
    type Api = FileSystemApi;
}

/// Endpoint 模型的契约身份（contract id + exact ABI + 领域分类）。
///
/// 与 [`Service`] 并存：`Service` 是旧 binding（全局名字 → 单槽）的契约表达，
/// [`Contract`] 是 Endpoint（typed `Endpoint<FileSystem>`）的表达；两者数值同源
/// （`abi/filesystem.toml`），迁移期不强制二选一。
///
/// `ID` 与 `ABI` 是**两个不同的值**：`ID` = `KCOMP_FILESYSTEM_CONTRACT`
/// （契约身份，ASCII `"VFSCONTR"`），`ABI` = `KCOMP_FILESYSTEM_ABI`
/// （逐位布局指纹，ASCII `"FILESYST"`）。
impl Contract for FileSystem {
    const ID: u64 = KCOMP_FILESYSTEM_CONTRACT;
    const ABI: u64 = KCOMP_FILESYSTEM_ABI;
    const KIND: InterfaceKind = InterfaceKind::Service;
}

// -----------------------------------------------------------------------
// provider wrapper：纯 Rust 实现 → SDK 生成 `#[repr(C)]` table
// -----------------------------------------------------------------------

/// filesystem 的 provider 接口：实现它，Direct 的 `#[repr(C)]` table 由 SDK 生成，
/// Gate 的扁平 method switch（[`dispatch::dispatch`]）也落到同一份实现。
///
/// 纯 Rust：无 `unsafe` / 无 `extern "C"` / 无裸指针。`Err` 侧是 `-Errno` 形式
/// （与 Core 导出、[`FileSystemApi`] 的返回约定一致；SDK 原样透传，不取反）。
///
/// 契约（两个 transport 逐方法一致）：
///
/// - `open` 的 `path` 必须是以 NUL 结尾、**只含一个 NUL**（结尾）的路径；
///   Gate 侧由扁平编码显式构造（内部 NUL / 缺结尾 NUL → `-EINVAL`），Direct 侧
///   的 C 字符串由 caller 保证。
/// - `read` 返回实际读到的字节数（`0` 表示 EOF / 空读），不得超过 `buf.len()`。
/// - 只读阶段：`open` 只接受 [`FILESYSTEM_OPEN_READ`]；写语义以 `-EROFS` 拒绝
///   （具体判定归 provider）。
pub trait FileSystemProvider {
    /// 挂载该 filesystem 实例（provider 自己决定具体语义；重复挂载按 provider 约定）。
    fn mount(&self) -> Result<()>;

    /// 卸载该 filesystem 实例。
    fn unmount(&self) -> Result<()>;

    /// 打开 `path`（NUL 结尾、相对该 filesystem root），返回不透明 file handle。
    ///
    /// 只读阶段只接受 [`FILESYSTEM_OPEN_READ`]；其它 flags → `-EROFS`。
    fn open(&self, path: &CStr, flags: u32) -> Result<u64>;

    /// 关闭一个 [`FileSystemProvider::open`] 返回的 handle。
    fn close(&self, handle: u64) -> Result<()>;

    /// 从 handle 当前位置读 `buf.len()` 字节；返回实际读到的字节数（`<= buf.len()`）。
    fn read(&self, handle: u64, buf: &mut [u8]) -> Result<usize>;
}

pub use service::FileSystemService;
