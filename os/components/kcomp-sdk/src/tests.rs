//! host 锚定测试：钉住 ABI 编码（`docs/architecture/driver-model.md` §6.2）。
//! Core 侧有对应测试 `component::export::tests::dma_direction_encoding_is_stable`。

mod heap;
mod macro_services;
mod mem;

#[test]
fn dma_direction_encoding_is_stable() {
    use crate::DmaDirection::{Bidirectional, FromDevice, ToDevice};
    assert_eq!(ToDevice.as_i32(), 0);
    assert_eq!(FromDevice.as_i32(), 1);
    assert_eq!(Bidirectional.as_i32(), 2);
}

/// `probe.result` 契约身份 / ABI 指纹 / create config 布局锚定（ASCII tag 的
/// 大端读数）：schema 数值漂移 = 组合期 `lookup` / `validate` 直接拒绝，这里钉死。
#[test]
fn probe_result_identity_is_anchored() {
    use crate::endpoint::Contract;
    use crate::probe::{
        KCOMP_DRIVER_CREATE_CONFIG_ABI, KCOMP_PROBE_OUTCOME_MATCH, KCOMP_PROBE_OUTCOME_NO_MATCH,
        KCOMP_PROBE_RESULT_ABI, KCOMP_PROBE_RESULT_CONTRACT, KCOMP_PROBE_RESULT_METHOD_RESULT,
        KCOMP_PROBE_RESULT_NAME, KCOMP_PROBE_RESULT_OUTPUT_LEN, ProbeResult,
    };
    assert_eq!(KCOMP_PROBE_RESULT_NAME, b"probe.result");
    assert_eq!(KCOMP_PROBE_RESULT_ABI, 0x5052_4F42_4950_4353);
    assert_eq!(KCOMP_PROBE_RESULT_CONTRACT, 0x5052_4243_4F4E_5452);
    assert_eq!(KCOMP_DRIVER_CREATE_CONFIG_ABI, 0x4452_5643_4F4E_4647);
    assert_eq!(KCOMP_PROBE_RESULT_METHOD_RESULT, 0);
    assert_eq!(KCOMP_PROBE_RESULT_OUTPUT_LEN, 8);
    assert_eq!(KCOMP_PROBE_OUTCOME_MATCH, 0);
    assert_eq!(KCOMP_PROBE_OUTCOME_NO_MATCH, 1);
    assert_eq!(ProbeResult::ID, KCOMP_PROBE_RESULT_CONTRACT);
    assert_eq!(ProbeResult::ABI, KCOMP_PROBE_RESULT_ABI);
}

/// 生命周期入口的 Rust 镜像：函数指针 = 指针宽（C 侧 `KcompTaskEntry` /
/// `kcomp_instance_create` 的 ABI 宽度由这里钉死）。
#[test]
fn lifecycle_entry_types_are_anchored() {
    use crate::abi::{KcompInstanceCreate, KcompInstanceDestroy, KcompTaskEntry};
    let ptr = core::mem::size_of::<usize>();
    assert_eq!(core::mem::size_of::<KcompTaskEntry>(), ptr);
    assert_eq!(core::mem::size_of::<KcompInstanceCreate>(), ptr);
    assert_eq!(core::mem::size_of::<KcompInstanceDestroy>(), ptr);
}

/// 当前 exact ABI 指纹；Core 校验组件 ELF 里的同名符号。
#[test]
fn kcomp_abi_fingerprint_is_anchored() {
    let abi = crate::abi::KCOMP_ABI;
    assert_eq!(abi, 0xF091_A62D_39C8_740B);
}

use crate::errno::Errno;

/// errno 的 **SDK 侧解码路径**（`from_code()` / `name()`）锚定：Core 的
/// `code()` 与数值本身由 `kcomp_abi_drift.rs` / `make abi-check` 守卫。
#[test]
fn errno_decoding_keeps_its_contract() {
    // `from_code` 的输入是 ABI 返回形状（`0` / `-errno`）；正数不是合法输入，
    // 落回 EIO 兜底（与生成前行为逐位一致）。
    assert_eq!(Errno::from_code(-22), Errno::EINVAL);
    assert_eq!(Errno::from_code(22), Errno::EIO);
    assert_eq!(Errno::EINVAL.code(), -22);
    assert_eq!(Errno::EINVAL.name(), "EINVAL");
}
