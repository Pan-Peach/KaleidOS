//! 以普通组件身份，经公开 ABI 压测 park/unpark 与调度交接。
//!
//! CoreTest 不查看 TaskTable / CpuState；只用 `kcore_task_state` 观察 Core 真相，
//! 用 `kcore_task_park` / `kcore_task_unpark` 驱动任务。场景放在 trace 组之后，
//! 不扩张前一个调度 trace 用例的精确事件窗口。

use kcomp_sdk::abi::{
    KcompTaskEntry, kcore_sched_run, kcore_task_create, kcore_task_exit, kcore_task_park,
    kcore_task_start, kcore_task_state, kcore_task_unpark, kcore_task_yield,
};
use kcomp_sdk::errno::Errno;

use super::report::Checks;

const STATE_CREATED: i32 = 0;
const STATE_RUNNABLE: i32 = 1;
const STATE_BLOCKED: i32 = 3;
const STATE_EXITED: i32 = 4;

const PARK_STRESS_ROUNDS: usize = 64;
const HANDOFF_BUDGET: usize = 8;

#[repr(C)]
struct EarlyPermitState {
    waiter: u32,
    first_returns: usize,
    second_returns: usize,
    first_park_blocked: usize,
    second_park_blocked: usize,
    second_unparks: usize,
    errors: usize,
}

impl EarlyPermitState {
    const fn new() -> Self {
        Self {
            waiter: u32::MAX,
            first_returns: 0,
            second_returns: 0,
            first_park_blocked: 0,
            second_park_blocked: 0,
            second_unparks: 0,
            errors: 0,
        }
    }
}

/// `arg` 指向组件 create 栈帧中的 `EarlyPermitState`；`kcore_sched_run` 挂起该栈帧，
/// 直到任务结束或再次阻塞才返回，因此 backing 一直有效。单 CPU 协作调度保证一次
/// 只有一个任务访问它。
extern "C" fn early_waiter(arg: *mut ()) {
    let state = arg.cast::<EarlyPermitState>();
    unsafe {
        if kcore_task_park() != 0 {
            (*state).errors += 1;
        }
        (*state).first_returns += 1;

        // 启动前发出的两个 unpark 应合并为一个 permit：第一次 park 快速返回，
        // 第二次 park 必须真的阻塞，等 observer 再显式 unpark。
        if kcore_task_park() != 0 {
            (*state).errors += 1;
        }
        (*state).second_returns += 1;
        kcore_task_exit();
    }
    loop {
        core::hint::spin_loop();
    }
}

/// 与 [`early_waiter`] 共享同一状态；单 CPU 协作调度下不会并行访问。
extern "C" fn early_observer(arg: *mut ()) {
    let state = arg.cast::<EarlyPermitState>();
    let mut attempts = 0usize;

    while attempts < HANDOFF_BUDGET {
        let (waiter, first_returns, second_returns) = unsafe {
            (
                (*state).waiter,
                (*state).first_returns,
                (*state).second_returns,
            )
        };
        let waiter_state = unsafe { kcore_task_state(waiter) };

        if second_returns != 0 {
            // 第二个 park 只能在本 observer 明确 unpark 后返回。否则多余 permit
            // 没有被合并，或第一次 park 没有正确消费 permit。
            unsafe {
                if (*state).second_unparks == 0 {
                    (*state).errors += 1;
                }
            }
            break;
        }

        if first_returns == 0 && waiter_state == STATE_BLOCKED {
            // 错误实现可能把第一次 park 阻塞了。记录失败后唤醒它继续跑完，
            // 让 CoreTest 有机会报告完整结果，而不是留下永久阻塞任务。
            unsafe { (*state).first_park_blocked += 1 };
            if unsafe { kcore_task_unpark(waiter) } != 0 {
                unsafe { (*state).errors += 1 };
            }
        } else if first_returns == 1 && waiter_state == STATE_BLOCKED {
            // 第一次 park 已消费唯一 permit；第二次 park 应阻塞在这里。
            unsafe { (*state).second_park_blocked += 1 };
            if unsafe { kcore_task_unpark(waiter) } == 0 {
                unsafe { (*state).second_unparks += 1 };
            } else {
                unsafe { (*state).errors += 1 };
            }
        } else if waiter_state == STATE_EXITED {
            unsafe { (*state).errors += 1 };
            break;
        }

        // yield 可能因 RR cursor 选择当前任务而原地返回；重复尝试直到 waiter
        // 前进到预期状态，或达到小的上限。
        if unsafe { kcore_task_yield() } != 0 {
            unsafe { (*state).errors += 1 };
            break;
        }
        attempts += 1;
    }

    unsafe {
        if (*state).second_park_blocked == 0 || (*state).second_returns == 0 {
            (*state).errors += 1;
        }
        kcore_task_exit();
    }
    loop {
        core::hint::spin_loop();
    }
}

#[repr(C)]
struct StressState {
    waiter: u32,
    waiting_round: usize,
    resumed_rounds: usize,
    unpark_rounds: usize,
    errors: usize,
}

impl StressState {
    const fn new() -> Self {
        Self {
            waiter: u32::MAX,
            waiting_round: 0,
            resumed_rounds: 0,
            unpark_rounds: 0,
            errors: 0,
        }
    }
}

/// `arg` 指向 create 栈帧中的 `StressState`，存活到本组调度返回；单 CPU 下串行访问。
extern "C" fn stress_waiter(arg: *mut ()) {
    let state = arg.cast::<StressState>();
    for round in 1..=PARK_STRESS_ROUNDS {
        unsafe { (*state).waiting_round = round };
        if unsafe { kcore_task_park() } != 0 {
            unsafe { (*state).errors += 1 };
            break;
        }
        unsafe { (*state).resumed_rounds += 1 };
    }
    unsafe { kcore_task_exit() };
    loop {
        core::hint::spin_loop();
    }
}

/// 与 [`stress_waiter`] 共享状态；Core 当前是单 CPU 协作调度，不会并行访问。
extern "C" fn stress_notifier(arg: *mut ()) {
    let state = arg.cast::<StressState>();
    let waiter = unsafe { (*state).waiter };

    for round in 1..=PARK_STRESS_ROUNDS {
        let mut blocked = false;
        for _ in 0..HANDOFF_BUDGET {
            let current_state = unsafe { kcore_task_state(waiter) };
            if current_state == STATE_BLOCKED {
                blocked = true;
                break;
            }
            if current_state == STATE_EXITED || unsafe { kcore_task_yield() } != 0 {
                break;
            }
        }
        if !blocked {
            unsafe { (*state).errors += 1 };
            break;
        }

        let expected_round = unsafe { (*state).waiting_round };
        if expected_round != round {
            unsafe { (*state).errors += 1 };
        }
        if unsafe { kcore_task_unpark(waiter) } != 0 {
            unsafe { (*state).errors += 1 };
            break;
        }
        unsafe { (*state).unpark_rounds += 1 };

        // A yield 不保证一定切给另一个任务；如果 RR 先选回 notifier，就再让出，
        // 直到 waiter 完成本轮 park 返回，或达到上限。
        let mut resumed = false;
        for _ in 0..HANDOFF_BUDGET {
            if unsafe { (*state).resumed_rounds } >= round {
                resumed = true;
                break;
            }
            if unsafe { kcore_task_yield() } != 0 {
                break;
            }
        }
        if !resumed {
            unsafe { (*state).errors += 1 };
            break;
        }
    }

    unsafe { kcore_task_exit() };
    loop {
        core::hint::spin_loop();
    }
}

/// 失败时尽量唤醒并跑完已创建的任务，避免一个失败用例把后续 CoreTest 场景挂住。
fn recover(tasks: &[u32], max_runs: usize) {
    for _ in 0..max_runs {
        let mut work = false;
        for &task in tasks {
            match unsafe { kcore_task_state(task) } {
                STATE_BLOCKED => {
                    let _ = unsafe { kcore_task_unpark(task) };
                    work = true;
                }
                STATE_RUNNABLE => work = true,
                _ => {}
            }
        }
        if !work || unsafe { kcore_sched_run() } != 0 {
            break;
        }
        if tasks
            .iter()
            .all(|&task| unsafe { kcore_task_state(task) } == STATE_EXITED)
        {
            break;
        }
    }
}

fn create(entry: KcompTaskEntry, arg: *mut (), out: &mut u32) -> bool {
    unsafe { kcore_task_create(entry, arg, out) == 0 }
}

pub fn group(checks: &mut Checks) {
    checks.group("task park / unpark");

    // ABI 拒绝不存在的任务，不改动任何其他任务。
    checks.check(
        43,
        "task-unpark-missing",
        unsafe { kcore_task_unpark(u32::MAX) } == Errno::ESRCH.code(),
    );

    // 重复提前 unpark 合并成一张 permit；第一次 park 直返，第二次 park 阻塞，
    // observer 再唤醒它。这同时覆盖 permit 的快速路径与 one-shot 消费。
    let mut early = EarlyPermitState::new();
    let early_ptr = core::ptr::addr_of_mut!(early);
    let mut early_waiter_id = u32::MAX;
    let mut early_observer_id = u32::MAX;
    let waiter_created = create(early_waiter, early_ptr.cast(), &mut early_waiter_id);
    let observer_created =
        waiter_created && create(early_observer, early_ptr.cast(), &mut early_observer_id);
    if waiter_created {
        unsafe { (*early_ptr).waiter = early_waiter_id };
    }
    let early_unpark_a = waiter_created && unsafe { kcore_task_unpark(early_waiter_id) } == 0;
    let early_unpark_b = waiter_created && unsafe { kcore_task_unpark(early_waiter_id) } == 0;
    let remains_created = waiter_created
        && early_unpark_a
        && early_unpark_b
        && unsafe { kcore_task_state(early_waiter_id) } == STATE_CREATED;
    checks.check(44, "park-early-unpark-accepted", remains_created);

    let early_waiter_started = waiter_created && unsafe { kcore_task_start(early_waiter_id) } == 0;
    let early_observer_started = observer_created
        && early_waiter_started
        && unsafe { kcore_task_start(early_observer_id) } == 0;
    let early_started = early_waiter_started && early_observer_started;
    let early_run =
        (early_waiter_started || early_observer_started) && unsafe { kcore_sched_run() } == 0;
    let early_fast_path = unsafe {
        (*early_ptr).first_returns == 1
            && (*early_ptr).first_park_blocked == 0
            && (*early_ptr).errors == 0
    };
    checks.check(
        45,
        "park-early-permit-fast-path",
        early_run && early_fast_path,
    );
    let one_shot = unsafe {
        (*early_ptr).second_park_blocked == 1
            && (*early_ptr).second_unparks == 1
            && (*early_ptr).second_returns == 1
            && (*early_ptr).errors == 0
    };
    checks.check(46, "park-permit-consumed-once", early_run && one_shot);
    let early_tasks = [early_waiter_id, early_observer_id];
    let early_exited = early_started
        && early_tasks
            .iter()
            .all(|&id| unsafe { kcore_task_state(id) } == STATE_EXITED);
    checks.check(47, "park-early-task-completion", early_run && early_exited);
    recover(&early_tasks, 4);

    // 两个真实组件任务反复做：waiter park → notifier 观察 Blocked → unpark →
    // 调度恢复 waiter。初始先后顺序不作假设，notifier 通过 Core 状态查询协调。
    let mut stress = StressState::new();
    let stress_ptr = core::ptr::addr_of_mut!(stress);
    let mut waiter_id = u32::MAX;
    let mut notifier_id = u32::MAX;
    let waiter_created = create(stress_waiter, stress_ptr.cast(), &mut waiter_id);
    let notifier_created =
        waiter_created && create(stress_notifier, stress_ptr.cast(), &mut notifier_id);
    if waiter_created {
        unsafe { (*stress_ptr).waiter = waiter_id };
    }
    let waiter_started = waiter_created && unsafe { kcore_task_start(waiter_id) } == 0;
    let notifier_started = notifier_created && unsafe { kcore_task_start(notifier_id) } == 0;
    let stress_started = waiter_started && notifier_started;
    let stress_run = (waiter_started || notifier_started) && unsafe { kcore_sched_run() } == 0;

    let stress_counts = unsafe {
        (*stress_ptr).waiting_round == PARK_STRESS_ROUNDS
            && (*stress_ptr).resumed_rounds == PARK_STRESS_ROUNDS
            && (*stress_ptr).unpark_rounds == PARK_STRESS_ROUNDS
            && (*stress_ptr).errors == 0
    };
    checks.check(48, "park-unpark-64-rounds", stress_run && stress_counts);
    let stress_tasks = [waiter_id, notifier_id];
    let stress_exited = stress_started
        && stress_tasks
            .iter()
            .all(|&id| unsafe { kcore_task_state(id) } == STATE_EXITED);
    checks.check(
        49,
        "park-unpark-task-completion",
        stress_run && stress_exited,
    );
    recover(&stress_tasks, PARK_STRESS_ROUNDS + 4);
}
