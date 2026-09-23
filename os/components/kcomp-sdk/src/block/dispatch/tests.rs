//! `block::dispatch` 的 host 测试：畸形帧拒绝 / method 路由 / provider errno 透传 /
//! C↔Rust 线格式逐字节一致。

use super::*;
use crate::block::tests_support::{BlockMock, NeverCalled};

/// 用切片直接构造 `Call`（本模块是 `block` 内部：不需要 raw frame）。
fn call<'a>(args: &'a [u8], input: &'a [u8], output: &'a mut [u8]) -> Call<'a> {
    Call {
        args,
        input,
        output,
    }
}

/// 畸形帧一律 `-EINVAL` 且 provider 不被调用（`NeverCalled` panic 即证据）。
#[test]
fn malformed_frames_are_rejected_before_the_provider() {
    let never = NeverCalled;
    let lba = encode_lba(1);
    let mut out = [0u8; 512];
    let mut out8 = [0u8; 8];

    // capacity：args / input / output 长度都必须精确。
    assert_eq!(
        dispatch(
            &never,
            KCOMP_BLOCK_METHOD_CAPACITY,
            call(&lba, &[], &mut out8)
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_BLOCK_METHOD_CAPACITY,
            call(&[], &[0u8; 1], &mut out8)
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_BLOCK_METHOD_CAPACITY,
            call(&[], &[], &mut [0u8; 7])
        ),
        Errno::EINVAL.code()
    );

    // read：args 必须恰好 8 字节；input 必须空；output 非零且 512 整数倍。
    assert_eq!(
        dispatch(&never, KCOMP_BLOCK_METHOD_READ, call(&[], &[], &mut out)),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_BLOCK_METHOD_READ,
            call(&lba, &[0u8; 512], &mut out)
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(&never, KCOMP_BLOCK_METHOD_READ, call(&lba, &[], &mut [])),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_BLOCK_METHOD_READ,
            call(&lba, &[], &mut [0u8; 513])
        ),
        Errno::EINVAL.code()
    );

    // write：output 必须空；input 非零且 512 整数倍。
    assert_eq!(
        dispatch(
            &never,
            KCOMP_BLOCK_METHOD_WRITE,
            call(&lba, &[0u8; 512], &mut out8)
        ),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(&never, KCOMP_BLOCK_METHOD_WRITE, call(&lba, &[], &mut [])),
        Errno::EINVAL.code()
    );
    assert_eq!(
        dispatch(
            &never,
            KCOMP_BLOCK_METHOD_WRITE,
            call(&lba, &[0u8; 513], &mut [])
        ),
        Errno::EINVAL.code()
    );
}

/// 未知 method → `-ENOSYS`（能力缺失，不是畸形帧）。
#[test]
fn unknown_method_is_enosys() {
    let device = BlockMock::new(8, None);
    assert_eq!(
        dispatch(&device, 99, call(&[], &[], &mut [])),
        Errno::ENOSYS.code()
    );
}

/// 合法帧：三个方法都落到 provider；read 回填、write 收到输入、capacity LE 编码。
#[test]
fn well_formed_frames_reach_the_provider() {
    let device = BlockMock::new(0x1234_5678_9ABC_DEF0, None);
    let mut out8 = [0u8; 8];
    assert_eq!(
        dispatch(
            &device,
            KCOMP_BLOCK_METHOD_CAPACITY,
            call(&[], &[], &mut out8)
        ),
        0
    );
    assert_eq!(u64::from_le_bytes(out8), 0x1234_5678_9ABC_DEF0);

    let lba = encode_lba(3);
    let mut out = [0u8; 512];
    assert_eq!(
        dispatch(&device, KCOMP_BLOCK_METHOD_READ, call(&lba, &[], &mut out)),
        0
    );
    assert_eq!(out, [0xA5; 512]);

    assert_eq!(
        dispatch(
            &device,
            KCOMP_BLOCK_METHOD_WRITE,
            call(&lba, &[0x5A; 512], &mut [])
        ),
        0
    );
}

/// 后端 `Err(e)` 原样透传为 `-errno`（不变成传输失败）。
#[test]
fn provider_errno_passes_through() {
    let device = BlockMock::new(8, Some(Errno::EIO));
    let lba = encode_lba(0);
    assert_eq!(
        dispatch(
            &device,
            KCOMP_BLOCK_METHOD_READ,
            call(&lba, &[], &mut [0u8; 512])
        ),
        Errno::EIO.code()
    );
    assert_eq!(
        dispatch(
            &device,
            KCOMP_BLOCK_METHOD_WRITE,
            call(&lba, &[0u8; 512], &mut [])
        ),
        Errno::EIO.code()
    );
}

/// **C/Rust 逐字节一致**：`kcomp_block_read`（手写 C 包装，`include/kcomp_block.h`）
/// 与 Rust [`encode_lba`] 必须产生同一份 `args`。C 侧公式：
/// `args[i] = (uint8_t)(lba >> (8 * i))`（LE、不依赖宿主字节序）；C 头里的
/// `_Static_assert` 把同一向量钉在编译期。任何一侧改编码都会红。
#[test]
fn block_wire_format_matches_the_c_wrapper_encoding() {
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
    for (lba, bytes) in C_ENCODING_CASES {
        assert_eq!(encode_lba(lba), bytes, "lba={lba:#x} 的 args 编码漂移");
        assert_eq!(decode_lba(&bytes), Some(lba));
    }
    // 方法号 / 长度也是 C 与 Rust 共用的同一份生成常量（`abi/block.toml`）。
    assert_eq!(KCOMP_BLOCK_METHOD_CAPACITY, 0);
    assert_eq!(KCOMP_BLOCK_METHOD_READ, 1);
    assert_eq!(KCOMP_BLOCK_METHOD_WRITE, 2);
    assert_eq!(KCOMP_BLOCK_LBA_LEN, 8);
    assert_eq!(KCOMP_BLOCK_CAPACITY_LEN, 8);
    // 长度不对的 args 必须拒绝（不是截断 / 补零）。
    assert_eq!(decode_lba(&[0u8; 7]), None);
    assert_eq!(decode_lba(&[0u8; 9]), None);

    // 一个真实的 read 调用把同一份字节送进 `kcore_endpoint_call` 的 args。
    let device = BlockMock::new(8, None);
    let mut out = [0u8; 512];
    assert_eq!(
        dispatch(
            &device,
            KCOMP_BLOCK_METHOD_READ,
            call(&encode_lba(0x0102_0304_0506_0708), &[], &mut out)
        ),
        0
    );
}
