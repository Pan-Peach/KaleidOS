//! host 锚定测试：钉住 ABI 编码（`docs/driver-model.md` §6.2）。
//! Core 侧有对应测试 `component::export::tests::dma_direction_encoding_is_stable`。

#[test]
fn dma_direction_encoding_is_stable() {
    use crate::DmaDirection::{Bidirectional, FromDevice, ToDevice};
    assert_eq!(ToDevice.as_i32(), 0);
    assert_eq!(FromDevice.as_i32(), 1);
    assert_eq!(Bidirectional.as_i32(), 2);
}

/// InterfaceKind ABI 编码锚定（与 Core `export.rs::kind_from_u32` 一致）。
#[test]
fn interface_kind_encoding_is_stable() {
    use crate::binding::InterfaceKind::{Device, Policy, Service};
    assert_eq!(Device.as_u32(), 0);
    assert_eq!(Service.as_u32(), 1);
    assert_eq!(Policy.as_u32(), 2);
}

/// SchedulerPolicy ABI fingerprint 锚定：与 Core `sched::SCHEDULER_POLICY_ABI`
/// 必须是同一数值（A/B 双侧手工锚定）。
#[test]
fn scheduler_policy_abi_is_anchored() {
    assert_eq!(
        crate::binding::SCHEDULER_POLICY_ABI.raw(),
        0x5343_4845_4455_4C52
    );
}

/// driver.prober ABI fingerprint 锚定（ASCII "DRVPROBE"）：prober 与 driver
/// 由完全相同的契约编译——数值漂移会让 bind 直接拒绝，这里把它钉死。
#[test]
fn driver_prober_abi_is_anchored() {
    assert_eq!(
        crate::binding::DRIVER_PROBER_ABI.raw(),
        0x4452_5650_524F_4245
    );
}

/// 分配接口名字锚定：publish / bind 两侧必须逐字节一致。
#[test]
fn driver_prober_name_is_anchored() {
    assert_eq!(crate::binding::DRIVER_PROBER_NAME, b"driver.prober");
}

/// `report_attempt` outcome 编码锚定（0 = Match，1 = NoMatch）。
#[test]
fn assign_outcome_encoding_is_stable() {
    assert_eq!(crate::binding::ASSIGN_MATCH, 0);
    assert_eq!(crate::binding::ASSIGN_NO_MATCH, 1);
}
