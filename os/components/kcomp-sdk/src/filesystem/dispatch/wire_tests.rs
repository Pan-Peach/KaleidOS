//! **C/Rust 逐字节一致**：C 包装（`include/kcomp_filesystem.h`）与 Rust 的
//! `encode_handle` / `encode_flags` / `encode_read_len` 必须产生同一份字节。

use super::*;

/// C 侧公式：`byte(i) = (uint8_t)(value >> (8 * i))`（LE、不依赖宿主字节序）；C 头
/// 里的 `_Static_assert` 把同一向量钉在编译期。任何一侧改编码都会红。
#[test]
fn filesystem_wire_format_matches_the_c_wrapper_encoding() {
    const C_ENCODING_CASES: [(u64, [u8; 8]); 4] = [
        (0, [0, 0, 0, 0, 0, 0, 0, 0]),
        (
            0x0102_0304_0506_0708,
            [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01],
        ),
        (
            0x1122_3344_5566_7788,
            [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
        ),
        (u64::MAX, [0xFF; 8]),
    ];
    for (value, bytes) in C_ENCODING_CASES {
        assert_eq!(encode_handle(value), bytes, "handle={value:#x} 编码漂移");
        assert_eq!(decode_handle(&bytes), Some(value));
        assert_eq!(encode_read_len(value as usize), bytes, "长度头编码漂移");
        assert_eq!(decode_read_len(&bytes), Some(value as usize));
    }
    assert_eq!(encode_flags(0x0102_0304), [0x04, 0x03, 0x02, 0x01]);
    assert_eq!(decode_flags(&[0x04, 0x03, 0x02, 0x01]), Some(0x0102_0304));

    // 方法号 / 长度 / 路径上限是 C 与 Rust 共用的同一份生成常量。
    assert_eq!(KCOMP_FILESYSTEM_METHOD_MOUNT, 0);
    assert_eq!(KCOMP_FILESYSTEM_METHOD_UNMOUNT, 1);
    assert_eq!(KCOMP_FILESYSTEM_METHOD_OPEN, 2);
    assert_eq!(KCOMP_FILESYSTEM_METHOD_CLOSE, 3);
    assert_eq!(KCOMP_FILESYSTEM_METHOD_READ, 4);
    assert_eq!(KCOMP_FILESYSTEM_HANDLE_LEN, 8);
    assert_eq!(KCOMP_FILESYSTEM_FLAGS_LEN, 4);
    assert_eq!(KCOMP_FILESYSTEM_READ_HEADER_LEN, 8);
    assert_eq!(KCOMP_FILESYSTEM_PATH_MAX, 256);
    assert_eq!(crate::filesystem::FILESYSTEM_OPEN_READ, 1);

    // 长度不对的 args 必须拒绝（不是截断 / 补零）。
    assert_eq!(decode_handle(&[0u8; 7]), None);
    assert_eq!(decode_handle(&[0u8; 9]), None);
    assert_eq!(decode_flags(&[0u8; 3]), None);
    assert_eq!(decode_flags(&[0u8; 5]), None);
    assert_eq!(decode_read_len(&[0u8; 7]), None);
}
