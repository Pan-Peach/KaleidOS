use super::tests::call;
use super::*;
use crate::filesystem::FileSystemService;
use crate::filesystem::tests_support::{FileSystemMock, MOCK_HANDLE, NeverCalled};

#[test]
fn node_adapters_share_direct_and_gate_semantics() {
    static SERVICE: FileSystemService<FileSystemMock> =
        FileSystemService::new(FileSystemMock::new(None));
    let provider = FileSystemMock::new(None);
    let mut args = [0; 12];
    args[..8].copy_from_slice(&MOCK_HANDLE.to_le_bytes());
    args[8..].copy_from_slice(&1u32.to_le_bytes());
    let mut out = [0; 8];
    assert_eq!(
        dispatch(
            &provider,
            KCOMP_FILESYSTEM_METHOD_ROOT,
            call(&[], &[], &mut out)
        ),
        0
    );
    assert_eq!(u64::from_le_bytes(out), MOCK_HANDLE);
    assert_eq!(
        dispatch(
            &provider,
            KCOMP_FILESYSTEM_METHOD_LOOKUP,
            call(&args, b"A", &mut out)
        ),
        0
    );
    assert_eq!(u64::from_le_bytes(out), MOCK_HANDLE + 1);
    let mut kind = [0; 4];
    assert_eq!(
        dispatch(
            &provider,
            KCOMP_FILESYSTEM_METHOD_NODE_INFO,
            call(&(MOCK_HANDLE + 1).to_le_bytes(), &[], &mut kind)
        ),
        0
    );
    assert_eq!(u32::from_le_bytes(kind), 1);

    let api = SERVICE.api();
    let mut root = 0;
    let mut node = 0;
    let mut kind = 0;
    // SAFETY: ctx 来自本 static service；输入与输出在调用期间有效。
    unsafe {
        assert_eq!((api.root)(SERVICE.ctx(), &mut root), 0);
        assert_eq!(
            (api.lookup)(SERVICE.ctx(), root, b"A".as_ptr(), 1, 1, &mut node),
            0
        );
        assert_eq!((api.node_info)(SERVICE.ctx(), node, &mut kind), 0);
    }
    assert_eq!((root, node, kind), (MOCK_HANDLE, MOCK_HANDLE + 1, 1));
}

#[test]
fn malformed_node_frames_never_call_the_provider() {
    let never = NeverCalled;
    let mut out = [0xa5; 8];
    for (method, args, input, len) in [
        (KCOMP_FILESYSTEM_METHOD_ROOT, &[][..], &b"A"[..], 8),
        (KCOMP_FILESYSTEM_METHOD_ROOT, &b"A"[..], &[][..], 8),
        (KCOMP_FILESYSTEM_METHOD_LOOKUP, &[0; 11][..], &b"A"[..], 8),
        (KCOMP_FILESYSTEM_METHOD_LOOKUP, &[0; 12][..], &[][..], 8),
        (KCOMP_FILESYSTEM_METHOD_LOOKUP, &[0; 12][..], &b"A"[..], 7),
        (KCOMP_FILESYSTEM_METHOD_NODE_INFO, &[0; 7][..], &[][..], 4),
        (KCOMP_FILESYSTEM_METHOD_NODE_INFO, &[0; 8][..], &b"A"[..], 4),
        (KCOMP_FILESYSTEM_METHOD_NODE_INFO, &[0; 8][..], &[][..], 3),
    ] {
        assert_eq!(
            dispatch(&never, method, call(args, input, &mut out[..len])),
            Errno::EINVAL.code()
        );
        assert_eq!(out, [0xa5; 8]);
    }
}
