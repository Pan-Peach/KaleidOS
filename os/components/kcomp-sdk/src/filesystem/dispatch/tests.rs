//! `filesystem::dispatch` 的 host 测试（畸形帧拒绝）。
//!
//! 本文件只放 frame 形状校验；合法帧行为在 `behaviour_tests`，C↔Rust 线格式在
//! `wire_tests`。

use super::*;
use crate::filesystem::tests_support::{FileSystemMock, MOCK_HANDLE, NeverCalled};

/// 用切片直接构造 `Call`（本模块是 `filesystem` 内部：不需要 raw frame）。
pub(super) fn call<'a>(args: &'a [u8], input: &'a [u8], output: &'a mut [u8]) -> Call<'a> {
    Call {
        args,
        input,
        output,
    }
}

/// `mount` / `unmount` / `close` / `read` 的畸形帧一律 `-EINVAL` 且 provider 不被
/// 调用（`NeverCalled` panic 即证据）。
#[test]
fn malformed_frames_are_rejected_before_the_provider() {
    let never = NeverCalled;
    let handle = encode_handle(MOCK_HANDLE);
    let mut out8 = [0u8; KCOMP_FILESYSTEM_HANDLE_LEN];
    let mut read_out = [0u8; KCOMP_FILESYSTEM_READ_HEADER_LEN + 32];

    // mount / unmount：args / input / output 都必须为空。
    for method in [
        KCOMP_FILESYSTEM_METHOD_MOUNT,
        KCOMP_FILESYSTEM_METHOD_UNMOUNT,
    ] {
        assert_eq!(
            dispatch(&never, method, call(&[0u8; 1], &[], &mut [])),
            Errno::EINVAL.code()
        );
        assert_eq!(
            dispatch(&never, method, call(&[], &[0u8; 1], &mut [])),
            Errno::EINVAL.code()
        );
        assert_eq!(
            dispatch(&never, method, call(&[], &[], &mut out8)),
            Errno::EINVAL.code()
        );
    }

    // close：args 恰好 8 字节；input / output 必须为空。
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_CLOSE,
            call(&[0u8; 7], &[], &mut [])
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_CLOSE,
            call(&handle, &[0u8; 1], &mut [])
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_CLOSE,
            call(&handle, &[], &mut out8)
        ),
        Errno::EINVAL.code()
    );

    // read：args 恰好 8 字节；input 必须空；output 至少含 8 字节头。
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_READ,
            call(&[0u8; 7], &[], &mut read_out)
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_READ,
            call(&handle, &[0u8; 1], &mut read_out)
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_READ,
            call(
                &handle,
                &[],
                &mut [0u8; KCOMP_FILESYSTEM_READ_HEADER_LEN - 1]
            )
        ),
        Errno::EINVAL.code()
    );
}

/// `open` 的畸形帧：flags 长度 / 路径有界且 NUL 结尾（唯一 NUL）/ output 恰好 8。
#[test]
fn malformed_open_frames_are_rejected_before_the_provider() {
    let never = NeverCalled;
    let flags = encode_flags(crate::filesystem::FILESYSTEM_OPEN_READ);
    let path = c"0:/HELLO.TXT".to_bytes_with_nul();
    let mut out8 = [0u8; KCOMP_FILESYSTEM_HANDLE_LEN];

    // flags 区必须是 4 字节。
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_OPEN,
            call(&[0u8; 3], path, &mut out8)
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_OPEN,
            call(&[0u8; 5], path, &mut out8)
        ),
        Errno::EINVAL.code()
    );

    // 空路径 / 缺结尾 NUL / 内部 NUL / 超长（超 PATH_MAX）都拒绝。
    for bad_path in [&b""[..], &b"0:/HELLO.TXT"[..], &b"0:/HEL\0LO.TXT\0"[..]] {
        assert_eq!(
            dispatch(
                &never,
                KCOMP_FILESYSTEM_METHOD_OPEN,
                call(&flags, bad_path, &mut out8)
            ),
            Errno::EINVAL.code(),
            "path {bad_path:?} 必须被拒"
        );
    }
    let mut too_long = [0x41u8; KCOMP_FILESYSTEM_PATH_MAX + 1];
    too_long[KCOMP_FILESYSTEM_PATH_MAX] = 0;
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_OPEN,
            call(&flags, &too_long, &mut out8)
        ),
        Errno::EINVAL.code()
    );

    // output 必须恰好 8 字节。
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_OPEN,
            call(&flags, path, &mut [0u8; 7])
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_FILESYSTEM_METHOD_OPEN,
            call(&flags, path, &mut [0u8; 9])
        ),
        Errno::EINVAL.code()
    );
}

/// 未知 method → `-ENOSYS`（能力缺失，不是畸形帧）。
#[test]
fn unknown_method_is_enosys() {
    let fs = FileSystemMock::new(None);
    assert_eq!(
        dispatch(&fs, 99, call(&[], &[], &mut [])),
        Errno::ENOSYS.code()
    );
}
