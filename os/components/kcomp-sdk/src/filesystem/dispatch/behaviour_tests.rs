//! `filesystem::dispatch` 的合法帧行为：method 路由、provider errno 透传、
//! 只读 flags、provider 违约防御。

use super::tests::call;
use super::*;
use crate::filesystem::tests_support::{FileSystemMock, MOCK_CONTENT, MOCK_HANDLE};

/// 合法帧：五个方法都落到 provider；open 回填 handle、read 回填头 + 数据。
#[test]
fn well_formed_frames_reach_the_provider() {
    let fs = FileSystemMock::new(None);
    let flags = encode_flags(crate::filesystem::FILESYSTEM_OPEN_READ);
    let path = c"0:/HELLO.TXT".to_bytes_with_nul();
    let handle = encode_handle(MOCK_HANDLE);

    assert_eq!(
        dispatch(&fs, KCOMP_FILESYSTEM_METHOD_MOUNT, call(&[], &[], &mut [])),
        0
    );

    let mut out8 = [0u8; KCOMP_FILESYSTEM_HANDLE_LEN];
    assert_eq!(
        dispatch(
            &fs,
            KCOMP_FILESYSTEM_METHOD_OPEN,
            call(&flags, path, &mut out8)
        ),
        0
    );
    assert_eq!(decode_handle(&out8), Some(MOCK_HANDLE));

    let mut out = [0u8; KCOMP_FILESYSTEM_READ_HEADER_LEN + 32];
    assert_eq!(
        dispatch(
            &fs,
            KCOMP_FILESYSTEM_METHOD_READ,
            call(&handle, &[], &mut out)
        ),
        0
    );
    let actual = decode_read_len(&out[..KCOMP_FILESYSTEM_READ_HEADER_LEN]).unwrap();
    assert_eq!(actual, MOCK_CONTENT.len());
    assert_eq!(
        &out[KCOMP_FILESYSTEM_READ_HEADER_LEN..KCOMP_FILESYSTEM_READ_HEADER_LEN + actual],
        MOCK_CONTENT,
        "数据从 offset 8 开始（8 字节头在数据之前）"
    );

    assert_eq!(
        dispatch(
            &fs,
            KCOMP_FILESYSTEM_METHOD_CLOSE,
            call(&handle, &[], &mut [])
        ),
        0
    );
    assert_eq!(
        dispatch(
            &fs,
            KCOMP_FILESYSTEM_METHOD_UNMOUNT,
            call(&[], &[], &mut [])
        ),
        0
    );
}

/// 后端 `Err(e)` 原样透传为 `-errno`（不变成传输失败）。
#[test]
fn provider_errno_passes_through() {
    let fs = FileSystemMock::new(Some(Errno::EIO));
    let handle = encode_handle(MOCK_HANDLE);
    assert_eq!(
        dispatch(&fs, KCOMP_FILESYSTEM_METHOD_MOUNT, call(&[], &[], &mut [])),
        Errno::EIO.code()
    );
    assert_eq!(
        dispatch(
            &fs,
            KCOMP_FILESYSTEM_METHOD_READ,
            call(
                &handle,
                &[],
                &mut [0u8; KCOMP_FILESYSTEM_READ_HEADER_LEN + 32]
            )
        ),
        Errno::EIO.code()
    );
}

/// 只读 flags：`open` 收到非 read flags → provider 的 `-EROFS` 透传。
#[test]
fn non_read_open_flags_pass_through_as_erofs() {
    let fs = FileSystemMock::new(None);
    let path = c"0:/HELLO.TXT".to_bytes_with_nul();
    assert_eq!(
        dispatch(
            &fs,
            KCOMP_FILESYSTEM_METHOD_OPEN,
            call(
                &encode_flags(0),
                path,
                &mut [0u8; KCOMP_FILESYSTEM_HANDLE_LEN]
            )
        ),
        Errno::EROFS.code()
    );
}

/// provider 返回超过数据容量的长度 = 契约违约 → `-EIO`（不 UB 兜底）。
#[test]
fn oversized_read_reply_is_eio() {
    struct OversizedRead;
    impl FileSystemProvider for OversizedRead {
        fn mount(&self) -> crate::errno::Result<()> {
            Ok(())
        }
        fn unmount(&self) -> crate::errno::Result<()> {
            Ok(())
        }
        fn open(&self, _path: &CStr, _flags: u32) -> crate::errno::Result<u64> {
            Ok(1)
        }
        fn close(&self, _handle: u64) -> crate::errno::Result<()> {
            Ok(())
        }
        fn read(&self, _handle: u64, buf: &mut [u8]) -> crate::errno::Result<usize> {
            Ok(buf.len() + 1)
        }
    }

    assert_eq!(
        dispatch(
            &OversizedRead,
            KCOMP_FILESYSTEM_METHOD_READ,
            call(
                &encode_handle(1),
                &[],
                &mut [0u8; KCOMP_FILESYSTEM_READ_HEADER_LEN + 4]
            )
        ),
        Errno::EIO.code()
    );
}
