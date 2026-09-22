//! 第 2 组（scheduling chain）：组件加载 → 接口可用 → 任务创建/启动 →
//! RR 调度 → yield/exit。
//!
//! 本组只断言 Core 自己的返回值与状态编码（`kcore_task_state`）；加载与调度的
//! “操作 → 事件”精确断言由 `trace` 组用本组返回的游标 / id 完成
//! （见 `runtime/trace.rs` 的身份限制说明）。

use kcomp_sdk::abi::{
    KcompTaskEntry, kcore_component_load, kcore_sched_run, kcore_task_count, kcore_task_create,
    kcore_task_exit, kcore_task_start, kcore_task_state, kcore_task_yield,
};
use kcomp_sdk::binding::{self, InterfaceKind, SCHEDULER_POLICY_ABI};
use kcomp_sdk::errno::Errno;

use super::report::Checks;
use super::trace;

/// `TaskState::Exited` 的编码（Core `kcore_task_state` 契约）。
const STATE_EXITED: i32 = 4;

/// 任务体迭代数：3 轮 yield 后各自计数必须为 3（host-testable 的纯常量）。
const EXPECTED_ITERS: usize = 3;

/// 本实例的可变状态：任务 A/B 的迭代计数（任务体只做计数 + yield；验证由本组
/// 在调度返回后读取）。
///
/// 旧实现是 `static mut A_COUNT` / `B_COUNT`（image-global）。按
/// `docs/component-lifecycle.md` §9，共享地址空间下 globals 仍是 image-global，
/// per-instance 状态必须来自显式分配：`kcomp_instance_create` 用
/// `kcore_heap_alloc` 分配本结构，指针经 `*out_state` 交 Core 保管，任务经
/// `kcore_task_create` 的 `arg` 拿回同一份。KernelNative 单 CPU，无并发。
#[repr(C)]
pub struct State {
    /// 任务 A 的迭代计数。
    pub a_count: usize,
    /// 任务 B 的迭代计数。
    pub b_count: usize,
}

extern "C" fn task_a(arg: *mut ()) {
    // SAFETY: `arg` 是 create 写入 `*out_state` 的同一分配，实例存活期间地址稳定
    //（Core 停止前不会释放）；任务是本实例的执行边界，单 CPU 无并发写者。
    let state = arg as *mut State;
    for _ in 0..EXPECTED_ITERS {
        unsafe {
            (*state).a_count += 1;
            kcore_task_yield();
        }
    }
    unsafe {
        kcore_task_exit();
    }
    // task_exit 永不返回本任务；防御性驻留（不可达）。
    loop {
        core::hint::spin_loop();
    }
}

extern "C" fn task_b(arg: *mut ()) {
    // SAFETY: 同 `task_a`。
    let state = arg as *mut State;
    for _ in 0..EXPECTED_ITERS {
        unsafe {
            (*state).b_count += 1;
            kcore_task_yield();
        }
    }
    unsafe {
        kcore_task_exit();
    }
    loop {
        core::hint::spin_loop();
    }
}

/// 本组结果：`trace` 组按这些 **Core 返回值** 做身份锚定（见 trace.rs）。
pub struct Outcome {
    /// `kcore_component_load` 之前取的 trace 游标。
    pub load_cursor: u64,
    /// `kcore_sched_run` 之前取的 trace 游标。
    pub run_cursor: u64,
    /// scheduler_rr 的 ComponentId（= load 返回值；身份限制见 trace.rs）。
    pub rr_id: i32,
    /// 两个任务的 TaskId（= task_create 返回值）。
    pub task_a: i32,
    pub task_b: i32,
}

pub fn group(checks: &mut Checks, state: *mut State) -> Outcome {
    checks.group("scheduling chain");

    // 加载 scheduler_rr（组件 → Core ABI → 加载链）。游标在操作前取样：
    // trace 组据此断言“这次加载”产生了 rr_id 的完整生命周期事件。
    let load_cursor = trace::cursor();
    let rr_id = unsafe { kcore_component_load(b"scheduler_rr".as_ptr(), b"scheduler_rr".len()) };
    checks.check(4, "scheduler-load", rr_id >= 0);

    // 任务创建：requester = core_test（Core 从 call_init 上下文解析），
    // entry = task_a/task_b（本组件镜像内的函数地址），`arg` = 本实例状态
    //（任务归属仍来自 Core 执行边界，与 `arg` 内容无关）。
    let mut a = 0u32;
    let mut b = 0u32;
    let a_ok = unsafe { kcore_task_create(task_a, state as *mut (), &mut a) } == 0;
    let b_ok = unsafe { kcore_task_create(task_b, state as *mut (), &mut b) } == 0;
    checks.check(5, "task-create", a_ok && b_ok && a != b);

    // 拒绝路径：entry 必须落在本组件**装载镜像内**（`[base, base+size)`）——
    // 镜像外的入口一律 -EFAULT，且被拒绝的创建不得留下任务（计数不变）。
    // 组件因此无法把执行权指向任意内核/别的组件地址。
    let tasks_before = unsafe { kcore_task_count() };
    let mut rogue_id = 0u32;
    let rogue = unsafe {
        // SAFETY: entry 是刻意构造的镜像外地址（`usize::MAX`）；Core 在触碰入口
        // 之前按 requester 装载镜像区间校验并拒绝（-EFAULT），不会调用它
        //（transmute 只把整数当函数地址传递，不产生调用）。arg 为空、out 可写。
        let entry = core::mem::transmute::<usize, KcompTaskEntry>(usize::MAX);
        kcore_task_create(entry, core::ptr::null_mut(), &mut rogue_id)
    };
    let tasks_after = unsafe { kcore_task_count() };
    checks.check(
        29,
        "task-entry-out-of-image",
        rogue == Errno::EFAULT.code() && tasks_after == tasks_before,
    );

    // 调度器接口已发布/绑定且 provider 存活（发布发生在 scheduler_rr 的 init）。
    let bound = binding::available(b"scheduler", InterfaceKind::Policy, SCHEDULER_POLICY_ABI);
    checks.check(6, "scheduler-bind", bound);

    // 启动（Created → Runnable），游标取样后进入调度：trace 窗口只含本次 run。
    unsafe {
        kcore_task_start(a);
        kcore_task_start(b);
    }
    let run_cursor = trace::cursor();
    let ran = unsafe { kcore_sched_run() } == 0;

    // 切换验证：A/B 各跑满 3 轮（RR 交替），计数必须各自为 3。
    // SAFETY: state 在实例存活期间地址稳定；两个任务已退出，无并发写者。
    let counts_ok =
        unsafe { (*state).a_count == EXPECTED_ITERS && (*state).b_count == EXPECTED_ITERS };
    checks.check(7, "task-switch", ran && counts_ok);

    // 退出验证：两个任务都已 Exited（终态由 Core 状态机提交）。
    let exited =
        unsafe { kcore_task_state(a) == STATE_EXITED && kcore_task_state(b) == STATE_EXITED };
    checks.check(8, "task-exit", exited);

    // scheduler_rr 仍 Ready（resolve 会做 provider 存活二次校验）。
    let rr_ready = binding::available(b"scheduler", InterfaceKind::Policy, SCHEDULER_POLICY_ABI);
    checks.check(9, "scheduler-rr", rr_ready && rr_id >= 0);

    // TODO(C5): 抢占链用例——两个"不 yield 的忙循环"任务被时钟强行切出
    //   （当前调度是协作式；timer 实现 + sched::on_timer_tick 接线后，
    //   在 scheduling chain 组追加 preempt check）。

    Outcome {
        load_cursor,
        run_cursor,
        rr_id,
        task_a: a as i32,
        task_b: b as i32,
    }
}
