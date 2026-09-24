//! 驱动链集成场景（吸收旧独立 `driver_prober` runner 的断言）：CoreTest 加载
//! **真实 `driver_prober` 组件**，让它按自己的策略枚举候选并 provisioning
//! `virtio_blk`，再用 Core 真相（trace 生命周期事件 / 设备归属 / endpoint
//! 服务）断言它的可观测行为。
//!
//! 流程（两段式）：
//!
//! 1. **候选枚举**：CoreTest 自己按 `virtio,mmio` 枚举、逐台 claim + 读
//!    VirtIO `DeviceID`（0x008）+ release，得到候选布局（数量 / 哪些是块设备 /
//!    第一台块设备的位置）——这是下面"停止条件"断言的参照真值。
//! 2. **准备**：取 trace 游标 → `kcore_component_create("driver_prober")`；它的
//!    dispatch 任务在 create 里创建/启动，必须在调度器里跑（monitor `load` 的
//!    语义相同）。
//! 3. **调度**（由 `runtime.rs` 统一 `kcore_sched_run`）：prober 逐台 create
//!    `virtio_blk`（assignment 经 create config，结果经 `probe.result` 拉取），
//!    首个 Match（成功 attach）后停止；没有 Match 时试完全部候选后干净结束。
//! 4. **断言**（全部来自 Core 真相）：分支在**机器拓扑事实**上——候选枚举给出的
//!    `first_blk` 是 Core 交付的真值，runner 只控制 QEMU 挂什么盘，CoreTest 不
//!    接收"场景"参数：
//!    - **有块设备**（default 拓扑）：窗口内 `ComponentState` 出生事件数 == 1
//!      （prober）+ 尝试数（= 第一台块设备的 ordinal + 1）→ 证明"枚举 → create
//!      → pull → Match/NoMatch → 首个 attach 后停止"整条无环流程真的发生；
//!      attached 的设备被 driver 持有（CoreTest `claim` → `-EBUSY`），其余候选
//!      已释放（claim 成功）；只有 attached 实例有 `block.device` endpoint，且
//!      它能真的读盘（capacity > 0、sector 0 的 MBR 签名 0xAA55）；对第二台块
//!      设备再 create 一个 `virtio_blk` → 驱动拒绝（`-EBUSY`：image-global 单
//!      attachment），且 attached 实例之后仍能读盘；
//!    - **无块设备**（no-block 拓扑）：prober 对**每个**候选 create 一个
//!      report-only `virtio_blk` 实例并 pull 到 NoMatch，试完全部候选后干净结束；
//!      断言出生事件数 == 1 + 候选数、每个实例都发布 `probe.result` 且 pull 到
//!      `outcome=1`、没有任何 `block.device` endpoint（没有 attach）、所有设备
//!      都未被残留持有（CoreTest 能再次认领）；
//!    - **stale NoMatch**（两种拓扑共有）：拿一台非 virtio-blk 的 MMIO 设备
//!      （goldfish RTC）当候选 create `virtio_blk` → pull 到 `outcome=1`、无
//!      block endpoint、claim 已释放（再 claim 成功）。
//!
//! prober 是组件、不是 Core：CoreTest 不重复它的目录/策略判断，只断言"它做了
//! 什么"能被 Core 观测到的部分。

use kcomp_sdk::abi::{self, KcompCreateArgs};
use kcomp_sdk::block::{BLOCK_DEVICE_NAME, BlockDevice};
use kcomp_sdk::endpoint::Endpoint;
use kcomp_sdk::errno::Errno;
use kcomp_sdk::klog;
use kcomp_sdk::probe::{self, DriverCreateConfig, ProbeReply, ProbeResult};

use super::report::Checks;
use super::trace;

/// prober / 驱动的组件镜像名。
const PROBER: &[u8] = b"driver_prober";
const VIRTIO_BLK: &[u8] = b"virtio_blk";

/// prober 的 coarse 路由键（不透明字节；prober 自己不解释它）。
const COMPATIBLE: &[u8] = b"virtio,mmio";
/// stale 候选用的非 virtio-blk MMIO 设备（NoMatch 路径）。
const STALE_COMPATIBLE: &[u8] = b"google,goldfish-rtc";

/// VirtIO MMIO `DeviceID` 寄存器偏移与块设备取值（VirtIO 规范）。
const VIRTIO_MMIO_DEVICE_ID_OFFSET: usize = 0x008;
const VIRTIO_ID_BLOCK: u32 = 2;

/// 候选设备容量上限（QEMU virt 至多 8 个 virtio-mmio transport）。
const MAX_CANDIDATES: u32 = 8;
/// trace 窗口内出生事件上限：prober + 每个候选一个实例。
const MAX_DECLARED: usize = 12;

/// block 传输单位与 MBR 签名偏移（与 runner 的 1 MiB 磁盘约定一致）。
const SECTOR: usize = 512;
const MBR_SIG_OFFSET: usize = 510;

/// 本场景组的结果与窗口（`prepare` 写、调度返回后 `report` 读）。
#[repr(C)]
pub struct State {
    /// `driver_prober` 实例 id（< 0 = 创建失败）。
    pub prober_id: i32,
    /// 准备阶段取的 trace 游标（窗口 = prober 创建 + dispatch 全程）。
    pub cursor: u64,
    /// 候选设备数量。
    pub candidate_count: u32,
    /// 候选里是 `virtio_blk` 的位图（bit i = ordinal i）。
    pub blk_mask: u32,
    /// 第一台块设备的 ordinal（`u32::MAX` = 没有块设备候选）。
    pub first_blk: u32,
}

const NO_ORDINAL: u32 = u32::MAX;

const fn empty_args() -> KcompCreateArgs {
    KcompCreateArgs {
        config_abi: 0,
        config: core::ptr::null(),
        config_len: 0,
    }
}

/// 直接读 32-bit MMIO 寄存器（识别候选用，Core 不参与）。
unsafe fn read_u32(base: *mut u8, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base as usize + offset) as *const u32) }
}

/// 按 ordinal 取候选 `DeviceId`（`-ENOENT` = 枚举完）。
fn nth(ordinal: u32) -> Result<u32, i32> {
    let mut device = 0u32;
    let rc = unsafe {
        abi::kcore_device_nth(COMPATIBLE.as_ptr(), COMPATIBLE.len(), ordinal, &mut device)
    };
    if rc == 0 { Ok(device) } else { Err(rc) }
}

/// 枚举 + coarse 识别候选（claim → 读 `DeviceID` → release）。返回枚举是否
/// 自洽：至少一个候选，且以 `-ENOENT` 正常终止。`first_blk == NO_ORDINAL`
/// 表示这台机器**没有块设备**（no-block 拓扑）——`report` 据此分支到 NoMatch
/// 路径，而不是 attach 路径。
fn enumerate_candidates(state: &mut State) -> bool {
    let mut count = 0u32;
    let mut mask = 0u32;
    let mut first_blk = NO_ORDINAL;
    let end_ok;

    loop {
        let mut device = 0u32;
        let rc = unsafe {
            abi::kcore_device_nth(COMPATIBLE.as_ptr(), COMPATIBLE.len(), count, &mut device)
        };
        if rc != 0 {
            end_ok = rc == Errno::ENOENT.code();
            break;
        }
        if count >= MAX_CANDIDATES {
            klog!("[core-test] too many virtio,mmio candidates");
            return false;
        }
        let (mut base, mut len) = (core::ptr::null_mut(), 0usize);
        if unsafe { abi::kcore_device_claim(device, &mut base, &mut len) } != 0 {
            klog!("[core-test] candidate claim failed (ordinal={})", count);
            return false;
        }
        // SAFETY: claim 交付本域 MMIO 窗口；DeviceID 是 VirtIO 规范寄存器。
        let device_id = unsafe { read_u32(base, VIRTIO_MMIO_DEVICE_ID_OFFSET) };
        let _ = unsafe { abi::kcore_device_release(device) };
        if device_id == VIRTIO_ID_BLOCK {
            mask |= 1u32 << count;
            if first_blk == NO_ORDINAL {
                first_blk = count;
            }
        }
        count += 1;
    }

    state.candidate_count = count;
    state.blk_mask = mask;
    state.first_blk = first_blk;
    count > 0 && end_ok
}

/// 第一段：枚举候选 + 加载真实 `driver_prober`（dispatch 任务留给 `sched_run`）。
pub fn prepare(checks: &mut Checks, state: &mut State) {
    checks.group("driver chain");
    checks.check(35, "driver-candidates", enumerate_candidates(state));

    state.cursor = trace::cursor();
    let mut prober = 0u32;
    let rc = unsafe {
        abi::kcore_component_create(PROBER.as_ptr(), PROBER.len(), &empty_args(), &mut prober)
    };
    if rc == 0 {
        state.prober_id = prober as i32;
    } else {
        klog!("[core-test] create driver_prober failed (rc={})", rc);
        state.prober_id = -1;
    }
    checks.check(36, "driver-prober-load", rc == 0);
}

/// 用 assignment 创建 `virtio_blk` 实例（assignment 经 create config 进入驱动，
/// 与 driver_prober 的做法同一扁平编码）。
fn create_driver(device_id: u32, result_name: &[u8], out_instance: &mut u32) -> i32 {
    let mut config_buf = [0u8; DriverCreateConfig::MAX_ENCODED_LEN];
    let Ok(config_len) = DriverCreateConfig::new(device_id, result_name).encode(&mut config_buf)
    else {
        return Errno::EINVAL.code();
    };
    let args = KcompCreateArgs {
        config_abi: probe::KCOMP_DRIVER_CREATE_CONFIG_ABI,
        config: config_buf.as_ptr().cast(),
        config_len,
    };
    unsafe {
        abi::kcore_component_create(VIRTIO_BLK.as_ptr(), VIRTIO_BLK.len(), &args, out_instance)
    }
}

/// attached 驱动实例的 `block.device` endpoint 真的能读盘：
/// bind（Direct）→ capacity > 0 → 读 sector 0 → MBR 签名 0xAA55；
/// 再用边界读校验**报告的容量确实等于设备的可寻址范围**
/// （`capacity - 1` 可读、`capacity` 越界拒绝）——比硬编码 runner 的磁盘大小更强。
fn attach_serves(endpoint: Endpoint<BlockDevice>) -> bool {
    let Ok(binding) = endpoint.bind() else {
        return false;
    };
    let Ok(capacity) = binding.capacity_sectors() else {
        return false;
    };
    if capacity == 0 {
        return false;
    }
    let mut sector = [0u8; SECTOR];
    binding.read(0, &mut sector).is_ok()
        && sector[MBR_SIG_OFFSET] == 0x55
        && sector[MBR_SIG_OFFSET + 1] == 0xAA
        && binding.read(capacity - 1, &mut sector).is_ok()
        && binding.read(capacity, &mut sector).is_err()
}

/// 设备归属：attached 的设备被 driver 持有（claim → `-EBUSY`），其余候选已释放
/// （claim 成功）。返回 (归属成立, attached 设备 id)。
fn check_ownership(state: &State) -> (bool, u32) {
    if state.first_blk == NO_ORDINAL {
        return (false, 0);
    }
    let mut ok = true;
    let mut attached = 0u32;
    for ordinal in 0..state.candidate_count {
        let Ok(device) = nth(ordinal) else {
            return (false, 0);
        };
        let (mut base, mut len) = (core::ptr::null_mut(), 0usize);
        let rc = unsafe { abi::kcore_device_claim(device, &mut base, &mut len) };
        if ordinal == state.first_blk {
            attached = device;
            ok &= rc == Errno::EBUSY.code();
        } else {
            ok &= rc == 0;
            if rc == 0 {
                let _ = unsafe { abi::kcore_device_release(device) };
            }
        }
    }
    (ok, attached)
}

/// 从 trace 窗口发现 attached 驱动实例（出生 id 里唯一有 `block.device` 的那个）。
fn find_attached(state: &State) -> (u32, Option<Endpoint<BlockDevice>>) {
    let mut declared = [0u32; MAX_DECLARED];
    let declared_len = trace::declared_components(state.cursor, &mut declared);
    let mut found = 0u32;
    let mut endpoint = None;
    for &id in &declared[..declared_len] {
        if id as i32 == state.prober_id {
            continue;
        }
        if let Ok(candidate) = Endpoint::<BlockDevice>::lookup(id, BLOCK_DEVICE_NAME) {
            found += 1;
            if endpoint.is_none() {
                endpoint = Some(candidate);
            }
        }
    }
    (found, endpoint)
}

/// stale 候选 → `virtio_blk` 的 NoMatch 路径：pull 到 `outcome=1`、无 block
/// endpoint、claim 已释放。
fn check_no_match() -> bool {
    let Ok(rtc) = nth_stale() else {
        klog!("[core-test] no stale MMIO candidate for NoMatch");
        return false;
    };
    let mut name_buf = [0u8; probe::RESULT_PORT_NAME_MAX];
    let Ok(name_len) = probe::result_port_name(1, &mut name_buf) else {
        return false;
    };
    let result_name = &name_buf[..name_len];
    let mut instance = 0u32;
    if create_driver(rtc, result_name, &mut instance) != 0 {
        return false;
    }
    let outcome_ok = match Endpoint::<ProbeResult>::lookup(instance, result_name) {
        Ok(endpoint) => {
            matches!(probe::pull_result(endpoint), Ok(reply) if reply.outcome == ProbeReply::NO_MATCH)
        }
        Err(_) => false,
    };
    let no_block = Endpoint::<BlockDevice>::lookup(instance, BLOCK_DEVICE_NAME).is_err();
    // report-only 实例必须释放 claim：CoreTest 能再次认领同一设备。
    let (mut base, mut len) = (core::ptr::null_mut(), 0usize);
    let released = unsafe { abi::kcore_device_claim(rtc, &mut base, &mut len) } == 0;
    if released {
        let _ = unsafe { abi::kcore_device_release(rtc) };
    }
    outcome_ok && no_block && released
}

/// `google,goldfish-rtc` 的 ordinal 0（stale 候选）。
fn nth_stale() -> Result<u32, i32> {
    let mut device = 0u32;
    let rc = unsafe {
        abi::kcore_device_nth(
            STALE_COMPATIBLE.as_ptr(),
            STALE_COMPATIBLE.len(),
            0,
            &mut device,
        )
    };
    if rc == 0 { Ok(device) } else { Err(rc) }
}

/// 多设备行为：对**第二台**块设备再 create 一个 `virtio_blk` → 驱动拒绝第二个
/// attachment（`-EBUSY`）。只有一台块设备时退化为对 attached 设备本身再 create
/// （claim 阶段即 `-EBUSY`）。
fn check_multi_device(state: &State, attached: u32) -> bool {
    let mut device = attached;
    for ordinal in 0..state.candidate_count {
        if ordinal == state.first_blk || state.blk_mask & (1u32 << ordinal) == 0 {
            continue;
        }
        if let Ok(candidate) = nth(ordinal) {
            device = candidate;
            break;
        }
    }
    let mut name_buf = [0u8; probe::RESULT_PORT_NAME_MAX];
    let Ok(name_len) = probe::result_port_name(2, &mut name_buf) else {
        return false;
    };
    let mut instance = 0u32;
    create_driver(device, &name_buf[..name_len], &mut instance) == Errno::EBUSY.code()
}

/// 该实例是否发布了 `probe.result` 且 pull 到 `NO_MATCH`（按 attempt 命名查找；
/// pull 是只读方法，prober 已拉过一次不影响再次读取）。
fn result_is_no_match(instance: u32) -> bool {
    for attempt in 1..=MAX_CANDIDATES {
        let mut name_buf = [0u8; probe::RESULT_PORT_NAME_MAX];
        let Ok(name_len) = probe::result_port_name(attempt, &mut name_buf) else {
            continue;
        };
        if let Ok(endpoint) = Endpoint::<ProbeResult>::lookup(instance, &name_buf[..name_len]) {
            return matches!(
                probe::pull_result(endpoint),
                Ok(reply) if reply.outcome == ProbeReply::NO_MATCH
            );
        }
    }
    false
}

/// no-block 拓扑（机器没有块设备）的 NoMatch 路径证据：每个候选都产生一个
/// report-only `virtio_blk` 实例，该实例发布 `probe.result` 且 pull 到
/// `NO_MATCH`，并且没有任何实例发布 `block.device` endpoint（**没有 attach**）。
fn no_match_evidence(state: &State, declared: &[u32]) -> bool {
    let mut drivers = 0u32;
    for &id in declared {
        if id as i32 == state.prober_id {
            continue;
        }
        drivers += 1;
        if Endpoint::<BlockDevice>::lookup(id, BLOCK_DEVICE_NAME).is_ok() {
            return false;
        }
        if !result_is_no_match(id) {
            return false;
        }
    }
    drivers == state.candidate_count
}

/// no-block 拓扑的归属证据：没有候选被残留持有（report-only 实例都已释放
/// claim），CoreTest 能再次认领每一台设备。
fn no_block_ownership(state: &State) -> bool {
    for ordinal in 0..state.candidate_count {
        let Ok(device) = nth(ordinal) else {
            return false;
        };
        let (mut base, mut len) = (core::ptr::null_mut(), 0usize);
        if unsafe { abi::kcore_device_claim(device, &mut base, &mut len) } != 0 {
            return false;
        }
        let _ = unsafe { abi::kcore_device_release(device) };
    }
    true
}

/// 第二段：调度返回后，用 Core 真相断言 prober / 驱动的可观测行为。
///
/// 拓扑分支：`first_blk == NO_ORDINAL` = 候选里没有块设备（runner 的 no-block
/// 机器）；否则至少有一台块设备（default 机器）。两个分支都要求 prober 完整
/// 走完自己的有限流程 + 所有出生实例达到 `Ready`。
pub fn report(checks: &mut Checks, state: &State) {
    let mut declared = [0u32; MAX_DECLARED];
    let declared_len = trace::declared_components(state.cursor, &mut declared);
    let no_block = state.first_blk == NO_ORDINAL;

    // prober 在首个 Match 后停止（有块设备：出生事件 = 1 + 第一台块设备的
    // ordinal + 1），没有 Match 时试完全部候选（出生事件 = 1 + 候选数）。
    // 每个出生实例还必须走完整生命周期（`Ready`）——旧 runner 的 `components`
    // `state=Ready` 断言的机器可读版本。
    let attempts = if no_block {
        state.candidate_count
    } else {
        state.first_blk + 1
    };
    let lifecycles_ok = declared[..declared_len]
        .iter()
        .all(|&id| trace::component_lifecycle(state.cursor, id as i32));
    checks.check(
        37,
        "driver-prober-dispatch",
        state.prober_id >= 0
            && state.candidate_count > 0
            && declared_len as u32 == 1 + attempts
            && lifecycles_ok,
    );

    if no_block {
        checks.check(
            38,
            "driver-attach",
            no_match_evidence(state, &declared[..declared_len]),
        );
        checks.check(39, "driver-no-match", check_no_match());
        checks.check(40, "driver-multi-device", no_block_ownership(state));
        return;
    }

    let (ownership_ok, attached_device) = check_ownership(state);
    let (endpoint_count, attached_endpoint) = find_attached(state);
    let attach_ok =
        ownership_ok && endpoint_count == 1 && attached_endpoint.is_some_and(attach_serves);
    checks.check(38, "driver-attach", attach_ok);

    checks.check(39, "driver-no-match", check_no_match());

    let multi_ok = check_multi_device(state, attached_device);
    // 被拒绝的第二次 attachment 不得复位已 attach 的设备：它仍能读盘。
    let still_serves = attached_endpoint.is_some_and(attach_serves);
    checks.check(40, "driver-multi-device", multi_ok && still_serves);
}
