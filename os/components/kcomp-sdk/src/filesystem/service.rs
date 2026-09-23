//! [`FileSystemService`]：把纯 Rust 的 [`FileSystemProvider`] 适配成
//! `FileSystemApi`（**Direct** transport）并发布 endpoint。
//!
//! # provider 不写 `unsafe extern "C"`
//!
//! function table 的布局、指针形态、`0/-errno` 编码都是 ABI 契约的一部分。让每个
//! provider 手写这张表，等于把 ABI 不变量和**裸指针前置条件**复制到每个 provider，
//! 写错一处就是 UB。本模块把这两件事收回 SDK：adapter 是**单态化**的，把裸指针收窄
//! 成 `&CStr` / `&mut [u8]`，再调 provider 的纯 Rust 方法。
//!
//! 发布时**同时**交付两种 transport（`docs/architecture/deployment.md` §2）：
//! `api` / `ctx`（Direct）+ `port`（Gate，经 image 的 `kcomp_service_dispatch`）；
//! **机制由 Core 在 bind 时选定**，provider 不选择。

use core::ffi::CStr;

use crate::abi;
use crate::endpoint::Contract;
use crate::errno::Errno;
use crate::filesystem::{FileSystem, FileSystemProvider};
use crate::generated::filesystem::FileSystemApi;

/// provider 实现与生成的 `#[repr(C)]` table 的配对；放进 `static` 后
/// [`publish_endpoint`](Self::publish_endpoint)。
///
/// `new` 是 `const fn`，可直接做 `static` 初始化：
///
/// ```text
/// static FS: FileSystemService<MyFs> = FileSystemService::new(MyFs { ... });
/// ```
pub struct FileSystemService<P: FileSystemProvider> {
    provider: P,
    api: FileSystemApi,
}

impl<P: FileSystemProvider> FileSystemService<P> {
    /// 由 `P` 生成 `#[repr(C)]` table：五个字段分别指向 `P` 的**单态化** adapter。
    pub const fn new(provider: P) -> Self {
        Self {
            provider,
            api: FileSystemApi {
                mount: mount::<P>,
                unmount: unmount::<P>,
                open: open::<P>,
                close: close::<P>,
                read: read::<P>,
            },
        }
    }

    /// 发布 `filesystem` **endpoint**（staged：只在 `kcomp_instance_create`
    /// 期间有效；Core 在 create 返回 0 后原子提交）。
    ///
    /// `port_name` 是组合策略分配的端点名（在 provider 实例内唯一）；`port` 是
    /// provider 定义的不透明 dispatch token——**Gate** 路径经 image 的
    /// `kcomp_service_dispatch` 用它选中本契约。发布同时交付 **Direct** 的
    /// `api` / `ctx`；机制由 Core 在 bind 时选定。
    ///
    /// # 为什么这是安全 fn
    ///
    /// `kcore_endpoint_publish` 是 `unsafe` extern：调用方可以递出一张与契约布局
    /// 不匹配的 table。这里不成立：`self.api` 不是外部数据，而是
    /// [`FileSystemService::new`] 从 `P` 生成的值（布局就是 `FileSystemApi` 类型
    /// 本身）；`ctx` 是 `'static` 实例里 provider 字段的地址（[`Self::ctx`]），
    /// 在 `'static` 内不会失效。两个 unsafe 前提都在本模块闭环。
    pub fn publish_endpoint(
        &'static self,
        port_name: &[u8],
        port: u32,
    ) -> crate::errno::Result<()> {
        // SAFETY: api 布局 = FileSystemApi（new 从 P 生成）；ctx = &'static
        // self.provider（地址稳定）；Core 只存指针、不解引用。
        let status = unsafe {
            abi::kcore_endpoint_publish(
                port_name.as_ptr(),
                port_name.len(),
                <FileSystem as Contract>::ID,
                <FileSystem as Contract>::KIND.as_u32(),
                <FileSystem as Contract>::ABI,
                port,
                (&self.api as *const FileSystemApi).cast(),
                self.ctx(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(Errno::from_code(status))
        }
    }

    /// provider 的 opaque `ctx` = `&self.provider`（Core 原样回传给 table 方法）。
    ///
    /// 地址稳定：`&'static self` 只可能来自 `static`（或泄漏的 `'static` 分配），
    /// 该内存此后不移动。测试 / 诊断用；正常 provider 不碰它。
    pub fn ctx(&'static self) -> *mut () {
        core::ptr::addr_of!(self.provider).cast_mut().cast()
    }

    /// 生成的 `#[repr(C)]` table（测试 / 诊断用；Core 拿到的就是它）。
    pub fn api(&self) -> &FileSystemApi {
        &self.api
    }
}

// -----------------------------------------------------------------------
// adapter：`FileSystemApi` 字段的实际函数（按 `::<P>` 单态化）
// -----------------------------------------------------------------------
//
// 必须是**模块层** fn：嵌套 fn 无法引用外层泛型参数，adapter 只能在这里定义、
// 在 new 里以 `::<P>` 实例化成 table 里的非泛型函数指针。

/// `Ok(())` → `0`；`Err(e)` → `e.code()`。
fn map(result: crate::errno::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
}

/// `FileSystemApi::mount` 的 adapter。
///
/// # Safety
/// `ctx` 必须是 [`FileSystemService::publish_endpoint`] 交付的 `&'static P`——
/// 本模块是该指针的唯一构造者，Core 只按 binding 原样回传。
unsafe extern "C" fn mount<P: FileSystemProvider>(ctx: *mut ()) -> i32 {
    // SAFETY: 见 Safety；ctx 恒为有效的 &'static P。
    let provider = unsafe { &*ctx.cast::<P>() };
    map(provider.mount())
}

/// `FileSystemApi::unmount` 的 adapter（Safety 同 [`mount`]）。
unsafe extern "C" fn unmount<P: FileSystemProvider>(ctx: *mut ()) -> i32 {
    // SAFETY: 同 mount。
    let provider = unsafe { &*ctx.cast::<P>() };
    map(provider.unmount())
}

/// `FileSystemApi::open` 的 adapter：校验入参、把 C 字符串收窄成 `&CStr`。
///
/// # Safety
/// 同 [`mount`]；此外调用方保证 `path` 在调用期间是有效的、以 NUL 结尾的 C 字符串
/// （`FileSystemApi` 契约），`out_handle` 可写。
unsafe extern "C" fn open<P: FileSystemProvider>(
    ctx: *mut (),
    path: *const u8,
    flags: u32,
    out_handle: *mut u64,
) -> i32 {
    if path.is_null() || out_handle.is_null() {
        return Errno::EINVAL.code();
    }
    // SAFETY: ctx 由本模块生成（= &'static P，见 mount 的 Safety）；path 非空且按
    // 契约指向 NUL 结尾的 C 字符串（只读扫描到 NUL）。
    let provider = unsafe { &*ctx.cast::<P>() };
    let path = unsafe { CStr::from_ptr(path.cast()) };
    match provider.open(path, flags) {
        Ok(handle) => {
            // SAFETY: out_handle 非空（上面已查），调用方保证可写。
            unsafe { *out_handle = handle };
            0
        }
        Err(error) => error.code(),
    }
}

/// `FileSystemApi::close` 的 adapter（Safety 同 [`mount`]）。
unsafe extern "C" fn close<P: FileSystemProvider>(ctx: *mut (), handle: u64) -> i32 {
    // SAFETY: 同 mount。
    let provider = unsafe { &*ctx.cast::<P>() };
    map(provider.close(handle))
}

/// `FileSystemApi::read` 的 adapter：校验出参、构造 slice、回写 `out_read`。
///
/// # Safety
/// 同 [`mount`]；此外调用方保证 `buf` 在调用期间有效（`len` 字节、按契约指向
/// Core 可见 RAM），`out_read` 可写。
unsafe extern "C" fn read<P: FileSystemProvider>(
    ctx: *mut (),
    handle: u64,
    buf: *mut u8,
    len: usize,
    out_read: *mut usize,
) -> i32 {
    // 契约入参在这里统一校验一次（provider 不重复校验）：null → -EINVAL。
    // `len == 0` 是合法请求（provider 返回 0），不是畸形入参。
    if buf.is_null() || out_read.is_null() {
        return Errno::EINVAL.code();
    }
    // SAFETY: ctx 由本模块生成（= &'static P）；buf 非空、len 字节在调用期间有效
    // 且独占可写。slice 只在 provider 调用期间存活，不逃逸。
    let provider = unsafe { &*ctx.cast::<P>() };
    let buf = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    match provider.read(handle, buf) {
        Ok(actual) if actual <= len => {
            // SAFETY: out_read 非空（上面已查），调用方保证可写。
            unsafe { *out_read = actual };
            0
        }
        // provider 返回超过 buffer 的长度 = 契约违约（不是 UB 兜底）。
        Ok(_) => Errno::EIO.code(),
        Err(error) => error.code(),
    }
}
