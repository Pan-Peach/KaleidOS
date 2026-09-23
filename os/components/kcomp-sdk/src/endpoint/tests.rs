//! `Endpoint<C>` 的 host 测试：用 `crate::test_support` 的 Core ABI 替身驱动
//! （真实 Core 链接不在 host 测试范围）。

use super::*;
use crate::block::BlockDevice;
use crate::generated::block::{KCOMP_BLOCK_DEVICE_ABI, KCOMP_BLOCK_DEVICE_CONTRACT};

/// 契约 id 不符的替身（其它字段同 `BlockDevice`）。
struct WrongContract;

impl Contract for WrongContract {
    const ID: u64 = KCOMP_BLOCK_DEVICE_CONTRACT ^ 1;
    const ABI: u64 = KCOMP_BLOCK_DEVICE_ABI;
    const KIND: InterfaceKind = InterfaceKind::Device;
}

/// ABI fingerprint 不符的替身。
struct WrongAbi;

impl Contract for WrongAbi {
    const ID: u64 = KCOMP_BLOCK_DEVICE_CONTRACT;
    const ABI: u64 = KCOMP_BLOCK_DEVICE_ABI ^ 1;
    const KIND: InterfaceKind = InterfaceKind::Device;
}

/// `BlockDevice` 的 Contract 身份锚定：id / abi / kind 都必须与 `abi/block.toml`
/// 生成物一致（任何漂移 = Core validate 直接拒绝）。
#[test]
fn block_device_contract_identity_is_anchored() {
    assert_eq!(<BlockDevice as Contract>::ID, KCOMP_BLOCK_DEVICE_CONTRACT);
    assert_eq!(<BlockDevice as Contract>::ABI, KCOMP_BLOCK_DEVICE_ABI);
    assert_eq!(<BlockDevice as Contract>::KIND, InterfaceKind::Device);
}

/// `from_id` 校验 contract + abi：匹配 → Ok（id 原样保留）；不匹配 → `EINVAL`；
/// 未知 id → `ENOENT`（由 Core 的 validate 档位决定）。
#[test]
fn from_id_validates_contract_abi_and_liveness() {
    let endpoint = Endpoint::<BlockDevice>::from_id(7).unwrap();
    assert_eq!(endpoint.id(), 7);
    assert_eq!(Endpoint::<BlockDevice>::from_id(0), Err(Errno::ENOENT));
    assert_eq!(
        Endpoint::<WrongContract>::from_id(7).err(),
        Some(Errno::EINVAL)
    );
    assert_eq!(Endpoint::<WrongAbi>::from_id(7).err(), Some(Errno::EINVAL));
}

/// `lookup` = 发现（只校验 contract + 存活）+ `from_id`（补齐 abi 校验）：
/// 成功给出 opaque id；发现失败原样透传（`ENOENT`）。
#[test]
fn lookup_discovers_then_validates() {
    let endpoint = Endpoint::<BlockDevice>::lookup(3, b"blk0").unwrap();
    assert_eq!(endpoint.id(), 304, "stub: id = provider * 100 + name_len");
    assert_eq!(
        Endpoint::<BlockDevice>::lookup(0, b"blk0").err(),
        Some(Errno::ENOENT)
    );
}

/// `Endpoint` 是 Copy 的值语义句柄（id 不可变；校验不重复）。
#[test]
fn endpoint_is_copy_and_id_is_immutable() {
    let a = Endpoint::<BlockDevice>::from_id(11).unwrap();
    let b = a;
    assert_eq!(a.id(), b.id());
    assert_eq!(a.id(), 11);
}
