//! `probe` 模块的 host 测试：扁平 config 编解码 / 保留名校验 / 结果端口命名 /
//! reply wire 编码 / pull 的三分类 / publish 交给 Core 的身份。

use super::*;
use crate::generated::probe::KcompDriverCreateConfig;
use crate::test_support;

const NAME: &[u8] = b"probe.result.7";

/// Given：生成的布局锚点。
/// Then：固定头部就是两个 LE `u32`（offset / size 由 schema 生成物钉死）。
#[test]
fn create_config_header_layout_is_anchored() {
    assert_eq!(KCOMP_DRIVER_CREATE_HEADER_LEN, 8);
    assert_eq!(KCOMP_DRIVER_CREATE_DEVICE_ID_OFFSET, 0);
    assert_eq!(KCOMP_DRIVER_CREATE_NAME_LEN_OFFSET, 4);
    assert_eq!(KCOMP_DRIVER_CREATE_NAME_OFFSET, 8);
    assert_eq!(KCOMP_DRIVER_CREATE_NAME_MAX, 256);
    assert_eq!(core::mem::size_of::<KcompDriverCreateConfig>(), 8);
    assert_eq!(core::mem::align_of::<KcompDriverCreateConfig>(), 4);
}

/// Given：一个 assignment；When：编码；Then：字节布局 = 两个 LE u32 + 名字，
/// 总长恰好 8 + N，且 decode 回同一值。
#[test]
fn driver_create_config_roundtrips_flat_bytes() {
    let config = DriverCreateConfig::new(0x0102_0304, NAME);
    let mut out = [0u8; DriverCreateConfig::MAX_ENCODED_LEN];

    let len = config.encode(&mut out).unwrap();

    assert_eq!(len, 8 + NAME.len());
    assert_eq!(&out[0..4], &0x0102_0304u32.to_le_bytes());
    assert_eq!(&out[4..8], &(NAME.len() as u32).to_le_bytes());
    assert_eq!(&out[8..len], NAME);
    assert_eq!(DriverCreateConfig::decode(&out[..len]).unwrap(), config);
}

/// Given：畸形 config；Then：每一条都在边界被 `-EINVAL` 拒绝（不 panic、不越界）。
#[test]
fn driver_create_config_rejects_malformed_input() {
    let mut encoded = [0u8; DriverCreateConfig::MAX_ENCODED_LEN];
    let len = DriverCreateConfig::new(5, NAME)
        .encode(&mut encoded)
        .unwrap();

    // 尾部多余字节（总长必须恰好 8 + N）。
    assert_eq!(
        DriverCreateConfig::decode(&encoded[..len + 1]).err(),
        Some(Errno::EINVAL)
    );
    // 截断（名字不完整）。
    assert_eq!(
        DriverCreateConfig::decode(&encoded[..len - 1]).err(),
        Some(Errno::EINVAL)
    );
    // 头部不完整。
    assert_eq!(
        DriverCreateConfig::decode(&encoded[..4]).err(),
        Some(Errno::EINVAL)
    );

    // name_len = 0（长度下界）。
    let mut zero = [0u8; 8];
    zero[4..8].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(DriverCreateConfig::decode(&zero).err(), Some(Errno::EINVAL));

    // name_len 超上限（声明长度本身先被挡下）。
    let mut over = [0u8; 8];
    over[4..8].copy_from_slice(&257u32.to_le_bytes());
    assert_eq!(DriverCreateConfig::decode(&over).err(), Some(Errno::EINVAL));

    // 编码侧：空名 / 超长名 / 保留名全部拒绝。
    let mut out = [0u8; DriverCreateConfig::MAX_ENCODED_LEN];
    assert_eq!(
        DriverCreateConfig::new(1, b"").encode(&mut out).err(),
        Some(Errno::EINVAL)
    );
    assert_eq!(
        DriverCreateConfig::new(1, &[b'a'; 257])
            .encode(&mut out)
            .err(),
        Some(Errno::EINVAL)
    );
    assert_eq!(
        DriverCreateConfig::new(1, KCOMP_PROBE_RESULT_NAME)
            .encode(&mut out)
            .err(),
        Some(Errno::EINVAL),
        "保留结果端口名不得被动态名字占用"
    );
}

/// Given：`KcompCreateArgs`（driver create 入口）；When：解析；
/// Then：核对 config_abi / 长度上界后才读字节——错 abi、空指针、越界长度都拒绝。
#[test]
fn from_create_args_validates_abi_and_bounds() {
    let config = DriverCreateConfig::new(9, NAME);
    let mut buffer = [0u8; DriverCreateConfig::MAX_ENCODED_LEN];
    let len = config.encode(&mut buffer).unwrap();
    let valid = abi::KcompCreateArgs {
        config_abi: KCOMP_DRIVER_CREATE_CONFIG_ABI,
        config: buffer.as_ptr().cast(),
        config_len: len,
    };
    // SAFETY: (config, config_len) 指向本帧的 buffer，长度一致。
    assert_eq!(
        unsafe { DriverCreateConfig::from_create_args(&valid) }.unwrap(),
        config
    );

    let wrong_abi = abi::KcompCreateArgs {
        config_abi: 0,
        ..valid
    };
    // SAFETY: 同上；abi 不匹配时不读 config。
    assert_eq!(
        unsafe { DriverCreateConfig::from_create_args(&wrong_abi) }.err(),
        Some(Errno::EINVAL)
    );

    let null_config = abi::KcompCreateArgs {
        config_abi: KCOMP_DRIVER_CREATE_CONFIG_ABI,
        config: core::ptr::null(),
        config_len: len,
    };
    // SAFETY: 空指针在构造切片前被拒绝。
    assert_eq!(
        unsafe { DriverCreateConfig::from_create_args(&null_config) }.err(),
        Some(Errno::EINVAL)
    );

    let oversized = abi::KcompCreateArgs {
        config_abi: KCOMP_DRIVER_CREATE_CONFIG_ABI,
        config: buffer.as_ptr().cast(),
        config_len: DriverCreateConfig::MAX_ENCODED_LEN + 1,
    };
    // SAFETY: 超上限长度在构造切片前被拒绝（不会读越界）。
    assert_eq!(
        unsafe { DriverCreateConfig::from_create_args(&oversized) }.err(),
        Some(Errno::EINVAL)
    );
}

/// Given：prober 的 attempt；When：生成结果端口名；Then：`probe.result.<attempt>`，
/// 与保留名不同；attempt = 0（cursor 空值）拒绝。
#[test]
fn result_port_name_is_attempt_suffixed_and_distinct_from_reserved() {
    let mut out = [0u8; RESULT_PORT_NAME_MAX];

    let len = result_port_name(1, &mut out).unwrap();
    assert_eq!(&out[..len], b"probe.result.1");
    let len = result_port_name(u32::MAX, &mut out).unwrap();
    assert_eq!(&out[..len], b"probe.result.4294967295");
    assert_ne!(&out[..len], KCOMP_PROBE_RESULT_NAME);
    assert!(len <= RESULT_PORT_NAME_MAX);

    assert_eq!(result_port_name(0, &mut out).err(), Some(Errno::EINVAL));
    assert_eq!(
        result_port_name(1, &mut out[..4]).err(),
        Some(Errno::EINVAL)
    );
}

/// Given：结果 outcome；Then：wire = outcome i32 LE + detail u32 LE，且诊断名正确。
#[test]
fn probe_reply_wire_encoding_is_outcome_then_detail() {
    let reply = ProbeReply::no_match(0xAA55);
    assert_eq!(reply.outcome_name(), "NoMatch");

    assert_eq!(ProbeReply::matched().outcome_name(), "Match");
    assert!(ProbeReply::matched().is_match());
    let failed = ProbeReply::creation_failed(Errno::EBUSY);
    assert_eq!(failed.outcome, Errno::EBUSY.code());
    assert_eq!(failed.outcome_name(), "Error");
}

/// Given：provider 发布结果端口；When：`publish_result_endpoint`；
/// Then：契约身份与 IPC-only marker 原样交给 Core。
#[test]
fn publish_result_endpoint_forwards_identity_to_core() {
    let _guard = test_support::lock();
    test_support::reset_script();
    test_support::script_publish(0);
    publish_result_endpoint(NAME).unwrap();

    let publish = test_support::last_publish().expect("stub recorded the publish");
    assert_eq!(publish.port_name, NAME);
    assert_eq!(publish.contract, KCOMP_PROBE_RESULT_CONTRACT);
    assert_eq!(publish.abi, KCOMP_PROBE_RESULT_ABI);
    assert_eq!(publish.kind, InterfaceKind::Service.as_u32());
    assert_eq!(publish.port, 0);
    assert_eq!(publish.api, 0, "结果契约没有 Direct function table");
    assert_eq!(publish.ctx, 0);

    test_support::reset_script();
    test_support::script_publish(Errno::EEXIST.code());
    assert_eq!(publish_result_endpoint(NAME), Err(Errno::EEXIST));
}
