//! 三类错误的区分（传输 / 方法 / 无效回复）+ Direct 错误映射 + 调用前端早拒。

use super::*;
use crate::filesystem::FileSystemApi;
use crate::generated::filesystem::KCOMP_FILESYSTEM_PATH_MAX;

/// 传输失败 / 方法失败 / 无意义回复，三者必须可区分（Gate 绑定）。
#[test]
fn transport_method_and_invalid_reply_are_distinguishable() {
    let _guard = test_support::lock();
    let mut region = [0u8; 8 + 32];

    test_support::reset_script();
    test_support::script_call(-2, 0);
    assert_eq!(
        gate_binding().read(0, &mut region),
        Err(InvokeError::Transport(Errno::ENOENT))
    );

    test_support::reset_script();
    test_support::script_call(0, -5);
    assert_eq!(
        gate_binding().read(0, &mut region),
        Err(InvokeError::Method(Errno::EIO))
    );

    test_support::reset_script();
    test_support::script_call(0, 7);
    assert_eq!(
        gate_binding().read(0, &mut region),
        Err(InvokeError::InvalidReply)
    );
}

/// Direct：provider 返回 `-errno` → `Method`；返回正数 → `InvalidReply`。
#[test]
fn direct_error_mapping_keeps_method_and_invalid_reply_apart() {
    let _guard = test_support::lock();

    unsafe extern "C" fn eio_mount(_ctx: *mut ()) -> i32 {
        Errno::EIO.code()
    }
    unsafe extern "C" fn bogus_mount(_ctx: *mut ()) -> i32 {
        7
    }
    static EIO_TABLE: FileSystemApi = FileSystemApi {
        mount: eio_mount,
        unmount: direct_unmount,
        open: direct_open,
        close: direct_close,
        read: direct_read,
    };
    static BOGUS_TABLE: FileSystemApi = FileSystemApi {
        mount: bogus_mount,
        unmount: direct_unmount,
        open: direct_open,
        close: direct_close,
        read: direct_read,
    };

    test_support::reset_script();
    test_support::script_bind(
        abi::KCORE_ENDPOINT_MECHANISM_DIRECT,
        &EIO_TABLE as *const FileSystemApi as usize,
        0,
    );
    assert_eq!(
        endpoint().bind().unwrap().mount(),
        Err(InvokeError::Method(Errno::EIO))
    );

    test_support::reset_script();
    test_support::script_bind(
        abi::KCORE_ENDPOINT_MECHANISM_DIRECT,
        &BOGUS_TABLE as *const FileSystemApi as usize,
        0,
    );
    assert_eq!(
        endpoint().bind().unwrap().mount(),
        Err(InvokeError::InvalidReply)
    );
}

/// 超长路径在调用前就被 typed 前端挡下（不浪费一次传输，两条机制一致）。
#[test]
fn oversized_path_is_rejected_without_a_call() {
    let _guard = test_support::lock();
    test_support::reset_script();
    test_support::script_call(0, 0);
    let binding = gate_binding();

    let long =
        std::ffi::CString::new(std::vec![b'a'; KCOMP_FILESYSTEM_PATH_MAX]).expect("无内部 NUL");
    assert_eq!(
        binding.open(long.as_c_str(), FILESYSTEM_OPEN_READ),
        Err(InvokeError::Method(Errno::EINVAL))
    );
    assert!(
        test_support::last_call().is_none(),
        "超长路径不得触发 kcore_endpoint_call"
    );
}

/// `read` 的缓冲区放不下 8 字节头 → 调用前挡下（两条机制一致）。
#[test]
fn undersized_read_buffer_is_rejected_without_a_call() {
    let _guard = test_support::lock();
    test_support::reset_script();
    test_support::script_call(0, 0);
    let binding = gate_binding();

    assert_eq!(
        binding.read(1, &mut [0u8; 7]),
        Err(InvokeError::Method(Errno::EINVAL))
    );
    assert!(
        test_support::last_call().is_none(),
        "畸形 read 缓冲区不得触发 kcore_endpoint_call"
    );
}
