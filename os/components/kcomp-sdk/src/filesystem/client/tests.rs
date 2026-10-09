//! `filesystem` typed 前端（`FileSystemBinding`）的 host 测试。
//!
//! 本文件：共享替身（Direct function table）+ Direct / Gate 路由 + `open` 编码；
//! 子模块按行为分簇：[`bind`]（bind 契约身份 / 拒绝）、[`errors`]（三类错误 +
//! 前端早拒）、[`gate_read`]（read 回复解码）。

use super::*;
use crate::abi;
use crate::filesystem::{FILESYSTEM_OPEN_READ, FileSystemApi};
use crate::generated::filesystem::{
    KCOMP_FILESYSTEM_METHOD_CLOSE, KCOMP_FILESYSTEM_METHOD_MOUNT, KCOMP_FILESYSTEM_METHOD_OPEN,
    KCOMP_FILESYSTEM_METHOD_UNMOUNT,
};
use crate::test_support;
use core::sync::atomic::{AtomicU32, Ordering};

mod bind;
mod errors;
mod gate_read;

/// Direct 路径的调用计数（"真的走了 function table"的证据）。
pub(super) static DIRECT_CALLS: AtomicU32 = AtomicU32::new(0);

/// Direct 替身返回的 handle / 数据。
pub(super) const DIRECT_HANDLE: u64 = 0x1122_3344_5566_7788;
pub(super) const DIRECT_DATA: &[u8] = b"direct fs data";

pub(super) unsafe extern "C" fn direct_mount(_ctx: *mut ()) -> i32 {
    DIRECT_CALLS.fetch_add(1, Ordering::SeqCst);
    0
}

pub(super) unsafe extern "C" fn direct_unmount(_ctx: *mut ()) -> i32 {
    DIRECT_CALLS.fetch_add(1, Ordering::SeqCst);
    0
}

pub(super) unsafe extern "C" fn direct_open(
    _ctx: *mut (),
    path: *const u8,
    flags: u32,
    out_handle: *mut u64,
) -> i32 {
    DIRECT_CALLS.fetch_add(1, Ordering::SeqCst);
    if path.is_null() || out_handle.is_null() {
        return Errno::EINVAL.code();
    }
    if flags != FILESYSTEM_OPEN_READ {
        return Errno::EROFS.code();
    }
    // SAFETY: 调用方（SDK typed 前端）保证 out_handle 可写。
    unsafe { *out_handle = DIRECT_HANDLE };
    0
}

pub(super) unsafe extern "C" fn direct_close(_ctx: *mut (), handle: u64) -> i32 {
    DIRECT_CALLS.fetch_add(1, Ordering::SeqCst);
    if handle == DIRECT_HANDLE {
        0
    } else {
        Errno::EBADF.code()
    }
}

pub(super) unsafe extern "C" fn direct_read(
    _ctx: *mut (),
    _handle: u64,
    buf: *mut u8,
    len: usize,
    out_read: *mut usize,
) -> i32 {
    DIRECT_CALLS.fetch_add(1, Ordering::SeqCst);
    if buf.is_null() || out_read.is_null() {
        return Errno::EINVAL.code();
    }
    let actual = len.min(DIRECT_DATA.len());
    // SAFETY: 调用方（SDK typed 前端）保证 buf 在调用期间可写、len 有效。
    unsafe {
        core::ptr::copy_nonoverlapping(DIRECT_DATA.as_ptr(), buf, actual);
        *out_read = actual;
    }
    0
}

pub(super) static DIRECT_TABLE: FileSystemApi = FileSystemApi {
    mount: direct_mount,
    unmount: direct_unmount,
    open: direct_open,
    close: direct_close,
    read: direct_read,
};

pub(super) fn endpoint() -> Endpoint<FileSystem> {
    Endpoint::<FileSystem>::from_id(7).expect("stub validate accepts id != 0 + filesystem contract")
}

/// 建立 Direct 绑定（Core 回复 DIRECT + 替身 table）。
pub(super) fn direct_binding() -> FileSystemBinding {
    let mut state = 0u8;
    let ctx = &mut state as *mut u8 as *mut ();
    test_support::script_bind(
        abi::KCORE_ENDPOINT_MECHANISM_DIRECT,
        &DIRECT_TABLE as *const FileSystemApi as usize,
        ctx as usize,
    );
    endpoint().bind().expect("stub bind returns DIRECT")
}

/// 建立 Gate 绑定（Core 回复 GATE，不交付 api/ctx）。
pub(super) fn gate_binding() -> FileSystemBinding {
    test_support::script_bind(abi::KCORE_ENDPOINT_MECHANISM_GATE, 0, 0);
    endpoint().bind().expect("stub bind returns GATE")
}

/// Direct：五个方法直调 function table（数据写入业务缓冲区），**绝不**经过
/// `kcore_endpoint_call`（稳态零 Core 介入、零打包）。
#[test]
fn direct_binding_calls_the_function_table_without_core_involvement() {
    let _guard = test_support::lock();
    test_support::reset_script();
    test_support::script_call(0, 0);

    let before = DIRECT_CALLS.load(Ordering::SeqCst);
    let binding = direct_binding();

    binding.mount().unwrap();
    let handle = binding.open(c"0:/HELLO.TXT", FILESYSTEM_OPEN_READ).unwrap();
    assert_eq!(handle, DIRECT_HANDLE);

    let mut region = [0u8; 8 + 32];
    let actual = binding.read(handle, &mut region).unwrap();
    assert_eq!(actual, DIRECT_DATA.len());
    assert_eq!(&region[..actual], DIRECT_DATA);

    binding.close(handle).unwrap();
    binding.unmount().unwrap();
    assert_eq!(DIRECT_CALLS.load(Ordering::SeqCst), before + 5);

    assert!(
        test_support::last_call().is_none(),
        "Direct 绑定绝不调用 kcore_endpoint_call"
    );
}

/// Gate：五个方法都经 `kcore_endpoint_call`，args / input 与 Direct 同一扁平编码。
#[test]
fn gate_binding_routes_every_call_through_kcore_endpoint_call() {
    let _guard = test_support::lock();
    let binding = gate_binding();
    let handle = 0x0102_0304_0506_0708u64;

    test_support::reset_script();
    test_support::script_call(0, 0);
    binding.mount().unwrap();
    let call = test_support::last_call().expect("stub recorded the gate call");
    assert_eq!(call.endpoint, 7);
    assert_eq!(call.method, KCOMP_FILESYSTEM_METHOD_MOUNT);
    assert!(call.args.is_empty() && call.input.is_empty() && call.output_len == 0);

    test_support::reset_script();
    test_support::script_call(0, 0);
    binding.close(handle).unwrap();
    let call = test_support::last_call().unwrap();
    assert_eq!(call.method, KCOMP_FILESYSTEM_METHOD_CLOSE);
    assert_eq!(call.args, handle.to_le_bytes());
    assert!(call.input.is_empty() && call.output_len == 0);

    test_support::reset_script();
    test_support::script_call(0, 0);
    binding.unmount().unwrap();
    assert_eq!(
        test_support::last_call().unwrap().method,
        KCOMP_FILESYSTEM_METHOD_UNMOUNT
    );
}

/// Gate `open`：args = 4 字节 LE flags，input = NUL 结尾路径（含结尾 NUL），
/// output = 8 字节 handle。
#[test]
fn gate_open_encodes_flags_and_null_terminated_path() {
    let _guard = test_support::lock();
    test_support::reset_script();
    test_support::script_call(0, 0);
    test_support::script_call_reply(&DIRECT_HANDLE.to_le_bytes());

    let handle = gate_binding()
        .open(c"0:/HELLO.TXT", FILESYSTEM_OPEN_READ)
        .unwrap();
    assert_eq!(handle, DIRECT_HANDLE);

    let call = test_support::last_call().unwrap();
    assert_eq!(call.method, KCOMP_FILESYSTEM_METHOD_OPEN);
    assert_eq!(call.args, [0x01, 0x00, 0x00, 0x00]);
    assert_eq!(call.input, b"0:/HELLO.TXT\0");
    assert_eq!(call.output_len, 8);
}
