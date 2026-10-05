//! trace 分组：把"trace 里有类似事件"升级为**"这个操作产生这个事件"**。
//!
//! 手法：受控操作**之前**取一次游标（`kcore_trace_stats` 的 `next_seq`），操作
//! **之后**从该游标读取事件，用操作返回的 id / handle 在载荷里做精确匹配：
//! - 组件生命周期：`kcore_component_load` 返回的 ComponentId ↔ `ComponentState.a`，
//!   且只接受 `Declared → Resolved → Starting → Ready` 这条确切序列；
//! - authority：claim / alloc 返回的 raw handle ↔ `ResourceGrant` /
//!   `ResourceRevoke.c`（`b` = 资源类别，`c` = raw handle，二者同时匹配）；
//! - 调度：`task_create` 返回的 TaskId ↔ `TaskSwitch.b` / `PolicyAccepted.b`，
//!   且要求窗口内每个提案都来自本次加载的 scheduler_rr、每个切换目标都是本测试
//!   创建的任务、`PolicyAccepted` 数与 `TaskSwitch` 数一致。
//!
//! # 身份模型的限制（不要把它读成完整证明）
//!
//! `ComponentId` 只在**一个 `Registry` 实例内**唯一（`registry.rs`），而 trace
//! ring 是进程/整机全局的：编号可能与其他 registry（host 测试每个用例一份；真机
//! 上 unload 后重载是全新实例、新 id）复用。本组能拿到的最强身份是"本组件刚加载
//! 的那个实例的 id"，且断言窗口从**该次 load 前的游标**开始——窗口内匹配到的事件
//! 只可能来自这次操作。彻底移除该限制需要身份跨 registry 全局唯一（全局单调
//! ComponentId / trace 记录带 boot epoch），那是 Core 的语义决定。
//!
//! core_test **无法**通过白名单拿到自己的 ComponentId（没有 self-id 导出），所以
//! "core_test 自己的 Starting"无法按 id 断言；本组改为对"core_test 自己执行的加载
//! 操作"做 id 锚定断言——身份来源同样是 Core 返回值。

use kcomp_sdk::abi::{
    ABSENT, KIND_COMPONENT_STATE, KIND_ENDPOINT_BIND, KIND_POLICY_ACCEPTED, KIND_RESOURCE_GRANT,
    KIND_RESOURCE_REVOKE, KIND_TASK_SWITCH, TraceRecordAbi, TraceStatsAbi, kcore_trace_read,
    kcore_trace_stats,
};

use super::report::Checks;
use super::resource;
use super::sched;

/// `ComponentState` 的 `to` 编码（Core `trace::abi::state_code` 的镜像）。
const STATE_DECLARED: u64 = 0;
const STATE_RESOLVED: u64 = 1;
const STATE_STARTING: u64 = 2;
const STATE_READY: u64 = 3;
#[cfg(target_arch = "riscv64")]
const STATE_FAILED: u64 = 6;

/// `ResourceKind` 的编码（Core `trace::abi::kind_code` 的镜像）。
const RESOURCE_DEVICE: u64 = 0;
const RESOURCE_IRQ: u64 = 1;
const RESOURCE_DMA: u64 = 2;

/// `EndpointBind` 的 mechanism 编码（Core `trace::abi::mechanism_code` 的镜像）。
pub const MECHANISM_DIRECT: u64 = 0;

/// 操作前取续读游标：`next_seq` = 下一条事件的 seq（读侧从它开始就不会看到旧事件）。
/// stats 读不到时返回 `u64::MAX`（扫描读不到任何记录 → 断言失败）：**失败要关闭**，
/// 不能退回“从 0 开始读”，那会把窗口放宽到操作之前。
pub fn cursor() -> u64 {
    stats().map_or(u64::MAX, |stats| stats.next_seq)
}

/// Trace 子系统状态（只读；`None` = 读取失败）。
fn stats() -> Option<TraceStatsAbi> {
    let mut stats = TraceStatsAbi {
        capacity: 0,
        oldest_seq: 0,
        next_seq: 0,
        overwritten_total: 0,
        enabled_mask: 0,
    };
    (unsafe { kcore_trace_stats(&mut stats) } == 0).then_some(stats)
}

/// 从 `from` 起按 seq 升序扫一遍存活记录（读到没有更多为止），每条交给 `visit`。
fn scan(from: u64, mut visit: impl FnMut(&TraceRecordAbi)) {
    let mut cursor = from;
    loop {
        let mut record = TraceRecordAbi {
            seq: 0,
            timestamp: 0,
            kind: 0,
            flags: 0,
            a: 0,
            b: 0,
            c: 0,
        };
        let mut next = 0u64;
        if unsafe { kcore_trace_read(cursor, &mut record, &mut next) } != 0 {
            break; // 负 = 读完了（ENOENT）；成功才是 0。
        }
        visit(&record);
        cursor = next;
    }
}

/// 窗口内每个 `EndpointBind` 事件：`visit(endpoint, mechanism)`。
///
/// 这是 Core 在 bind 时选定调用机制的**唯一可观测点**（运行期不再重决策），
/// 用来替代旧 runner "业务调用没有 gate dispatch 日志"的差分证据：
/// 业务读的绑定机制必须是 [`MECHANISM_DIRECT`]。
pub fn binds(from: u64, mut visit: impl FnMut(u64, u64)) {
    scan(from, |record| {
        if record.kind == KIND_ENDPOINT_BIND {
            visit(record.a, record.c);
        }
    });
}

/// 窗口内的**出生**事件（`ComponentState` 的 `from == ABSENT`）：每个
/// `kcore_component_create` / `kcore_component_load` 恰好一条。返回写入 `out`
/// 的组件 id 数（超出容量丢弃；容量不足时返回值会小于真实数量）。
pub fn declared_components(from: u64, out: &mut [u32]) -> usize {
    let mut count = 0usize;
    scan(from, |record| {
        if record.kind == KIND_COMPONENT_STATE && record.b == ABSENT && count < out.len() {
            out[count] = record.a as u32;
            count += 1;
        }
    });
    count
}

/// 读路径可用：从 Core 自己报告的 `oldest_seq` 起读，至少能读到一条记录。
fn readable() -> bool {
    let Some(stats) = stats() else {
        return false;
    };
    if stats.next_seq <= stats.oldest_seq {
        return false;
    }
    let mut seen = 0u32;
    scan(stats.oldest_seq, |_| seen += 1);
    seen > 0
}

/// 加载操作产出的**确切**生命周期：`id` 只允许按
/// `Declared → Resolved → Starting → Ready` 走一遍（多余/乱序转换都失败）。
pub fn component_lifecycle(from: u64, id: i32) -> bool {
    if id < 0 {
        return false;
    }
    const EXPECTED: [(u64, u64); 4] = [
        (ABSENT, STATE_DECLARED),
        (STATE_DECLARED, STATE_RESOLVED),
        (STATE_RESOLVED, STATE_STARTING),
        (STATE_STARTING, STATE_READY),
    ];
    let id = u64::from(id as u32);
    let mut step = 0usize;
    let mut ok = true;
    scan(from, |record| {
        if record.kind != KIND_COMPONENT_STATE || record.a != id {
            return;
        }
        if step < EXPECTED.len() && (record.b, record.c) == EXPECTED[step] {
            step += 1;
        } else {
            ok = false;
        }
    });
    ok && step == EXPECTED.len()
}

/// 指定实例在本次操作窗口内确实失败；不能用另一个组件的事件代替。
#[cfg(target_arch = "riscv64")]
pub fn component_failed(from: u64, id: u32) -> bool {
    let mut failures = 0;
    scan(from, |record| {
        if record.kind == KIND_COMPONENT_STATE
            && record.a == u64::from(id)
            && (record.b == STATE_STARTING || record.b == STATE_READY)
            && record.c == STATE_FAILED
        {
            failures += 1;
        }
    });
    failures == 1
}

/// `sched_run` 产出的确切切换：窗口内每个 `TaskSwitch` 的目标都是本测试创建的
/// 两个任务，每个 `PolicyAccepted` 都来自 `rr_id` 且落在两个任务上，
/// 且“采纳的提案数 == 发生的切换数”（多出/缺失都失败）。
fn sched_trace(from: u64, rr_id: i32, task_a: i32, task_b: i32) -> bool {
    if rr_id < 0 || task_a < 0 || task_b < 0 {
        return false;
    }
    let (a, b) = (u64::from(task_a as u32), u64::from(task_b as u32));
    let rr = u64::from(rr_id as u32);
    let mut switches = 0u32;
    let mut accepted = 0u32;
    let mut saw_a = false;
    let mut saw_b = false;
    let mut bad = false;
    scan(from, |record| match record.kind {
        KIND_TASK_SWITCH => {
            switches += 1;
            // from 只允许是锚点（ABSENT）或本测试的两个任务；to 必须是两者之一。
            if record.a != ABSENT && record.a != a && record.a != b {
                bad = true;
            }
            if record.b == a {
                saw_a = true;
            } else if record.b == b {
                saw_b = true;
            } else {
                bad = true; // 窗口内的切换目标必须都是本测试创建的任务。
            }
        }
        KIND_POLICY_ACCEPTED => {
            accepted += 1;
            if record.a != rr || (record.b != a && record.b != b) {
                bad = true; // 提案必须来自本次加载的 scheduler_rr 且落在两个任务上。
            }
        }
        _ => {}
    });
    !bad && switches > 0 && accepted == switches && saw_a && saw_b
}

/// 三条 authority 的 grant / revoke 事件都出现，且**类别 + raw handle 精确匹配**
/// （`c` = raw handle；handle 由对应操作返回，不是猜的）。
fn authority_events(from: u64, event_kind: u32, expected: &[(u64, u64)]) -> bool {
    let mut found = [false; 3];
    scan(from, |record| {
        if record.kind != event_kind {
            return;
        }
        for (index, &(resource_kind, handle)) in expected.iter().enumerate() {
            if record.b == resource_kind && record.c == handle {
                found[index] = true;
            }
        }
    });
    found.iter().all(|&hit| hit)
}

pub fn group(checks: &mut Checks, sched: &sched::Outcome, resource: &resource::Outcome) {
    checks.group("trace sequence");
    checks.check("trace-readable", readable());
    checks.check(
        "component-lifecycle",
        component_lifecycle(sched.load_cursor, sched.rr_id),
    );
    checks.check(
        "sched-trace",
        sched_trace(sched.run_cursor, sched.rr_id, sched.task_a, sched.task_b),
    );
    let handles = [
        (RESOURCE_DEVICE, resource.device),
        (RESOURCE_IRQ, resource.irq),
        (RESOURCE_DMA, resource.dma),
    ];
    checks.check(
        "authority-grant-trace",
        resource.grants_ok && authority_events(resource.cursor, KIND_RESOURCE_GRANT, &handles),
    );
    checks.check(
        "authority-revoke-trace",
        resource.revokes_ok && authority_events(resource.cursor, KIND_RESOURCE_REVOKE, &handles),
    );
}
