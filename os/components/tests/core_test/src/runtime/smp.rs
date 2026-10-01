//! SMP integration through public Core APIs. Tasks and orchestration live here;
//! only the deliberately failing owner is a separate component.
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::abi::{self, KcompCreateArgs, KcompTaskEntry};
use kcomp_sdk::endpoint::Endpoint;
use kcomp_sdk::scheduler::{SCHEDULER_POLICY_NAME, SchedulerPolicy};

use super::{report::Checks, trace};

const CONFIG_ABI: u64 = 0x534d_5050_414e_4943; // "SMPPANIC"
const EXITED: i32 = 4;

pub struct State {
    // u32 window: ready mask, turn, primary progress[2], helper progress[2],
    // primary task ids[2], done mask, spare, entry guards[2], helper task ids[2].
    words: [AtomicU32; 14],
    tasks: [TaskArgs; 2],
}

struct TaskArgs {
    words: *mut AtomicU32,
    cpu: u32,
}

#[repr(C)]
struct PanicConfig {
    words: *mut u32,
    cpu: u32,
}

fn word(args: &TaskArgs, index: usize) -> &AtomicU32 {
    // All accesses to this resident instance allocation are atomic. It is
    // reset only after both CPUs report all tasks Exited through the Core API.
    unsafe { &*args.words.add(index) }
}

fn deadline(seconds: u64) -> u64 {
    unsafe { abi::kcore_now().saturating_add(abi::kcore_timebase_hz() * seconds) }
}

fn check_cpu(cpu: u32) {
    assert_eq!(unsafe { abi::kcore_cpu_current() }, cpu);
}

fn rendezvous(args: &TaskArgs) {
    word(args, 0).fetch_or(1 << args.cpu, Ordering::AcqRel);
    let end = deadline(3);
    // Neither CPU yields here: completion requires actual parallel execution.
    while word(args, 0).load(Ordering::Acquire) != 3 {
        assert!(
            unsafe { abi::kcore_now() } < end,
            "SMP rendezvous timed out"
        );
        core::hint::spin_loop();
    }
}

extern "C" fn ping_pong(arg: *mut ()) {
    let args = unsafe { &*arg.cast::<TaskArgs>() };
    let cpu = args.cpu as usize;
    check_cpu(args.cpu);
    assert_eq!(word(args, 10 + cpu).swap(1, Ordering::AcqRel), 0);
    rendezvous(args);
    let end = deadline(3);
    for round in 0..128 {
        while word(args, 1).load(Ordering::Acquire) != args.cpu {
            assert!(unsafe { abi::kcore_now() } < end, "remote wake timed out");
            assert_eq!(unsafe { abi::kcore_task_park() }, 0);
            check_cpu(args.cpu);
        }
        word(args, 2 + cpu).fetch_add(1, Ordering::Release);
        word(args, 1).store(1 - args.cpu, Ordering::Release);
        if cpu == 0 || round != 127 {
            let peer = word(args, 7 - cpu).load(Ordering::Acquire);
            assert_eq!(unsafe { abi::kcore_task_unpark(peer) }, 0);
        }
        assert_eq!(unsafe { abi::kcore_task_yield() }, 0);
        check_cpu(args.cpu);
    }
    // Each helper must make progress while its peer task remains alive.
    assert!(word(args, 4 + cpu).load(Ordering::Acquire) > 0);
    word(args, 8).fetch_or(1 << cpu, Ordering::Release);
    assert_eq!(unsafe { abi::kcore_task_exit() }, 0);
    panic!("exited SMP task resumed");
}

extern "C" fn helper(arg: *mut ()) {
    let args = unsafe { &*arg.cast::<TaskArgs>() };
    for _ in 0..32 {
        check_cpu(args.cpu);
        word(args, 4 + args.cpu as usize).fetch_add(1, Ordering::Release);
        assert_eq!(unsafe { abi::kcore_task_yield() }, 0);
    }
    word(args, 8).fetch_or(1 << (2 + args.cpu), Ordering::Release);
    assert_eq!(unsafe { abi::kcore_task_exit() }, 0);
    panic!("exited SMP helper resumed");
}

extern "C" fn healthy(arg: *mut ()) {
    let args = unsafe { &*arg.cast::<TaskArgs>() };
    check_cpu(args.cpu);
    rendezvous(args);
    let end = deadline(3);
    let peer = word(args, 7 - args.cpu as usize).load(Ordering::Acquire);
    while unsafe { abi::kcore_task_state(peer) } != EXITED {
        assert!(unsafe { abi::kcore_now() } < end, "panic peer did not exit");
        core::hint::spin_loop();
    }
    // Work starts after Core has committed the other CPU's abort, so progress
    // cannot merely predate the failure under test.
    for _ in 0..128 {
        check_cpu(args.cpu);
        word(args, 2 + args.cpu as usize).fetch_add(1, Ordering::Release);
        assert_eq!(unsafe { abi::kcore_task_yield() }, 0);
    }
    word(args, 8).fetch_or(1 << args.cpu, Ordering::Release);
    assert_eq!(unsafe { abi::kcore_task_exit() }, 0);
    panic!("healthy SMP task resumed after exit");
}

fn reset(state: *mut State) {
    let words = unsafe { core::ptr::addr_of_mut!((*state).words).cast::<AtomicU32>() };
    // Raw writes also initialize the fresh State, without borrowing uninit data.
    for index in 0..14 {
        unsafe { words.add(index).write(AtomicU32::new(0)) };
    }
    for cpu in 0..2 {
        unsafe {
            core::ptr::addr_of_mut!((*state).tasks[cpu]).write(TaskArgs {
                words,
                cpu: cpu as u32,
            });
        }
    }
}

fn args(state: *mut State, cpu: usize) -> *mut TaskArgs {
    unsafe { core::ptr::addr_of_mut!((*state).tasks[cpu]) }
}

fn value(state: *mut State, slot: usize) -> u32 {
    word(unsafe { &*args(state, 0) }, slot).load(Ordering::Acquire)
}

fn start(state: *mut State, entry: KcompTaskEntry, cpu: usize, slot: usize) -> bool {
    let mut id = u32::MAX;
    if unsafe { abi::kcore_task_create(entry, args(state, cpu).cast(), &mut id) } != 0 {
        return false;
    }
    word(unsafe { &*args(state, cpu) }, slot).store(id, Ordering::Release);
    // Reject invalid placement without consuming Created -> Runnable.
    unsafe {
        abi::kcore_task_start_on(id, u32::MAX) == -22
            && abi::kcore_task_state(id) == 0
            && abi::kcore_task_start_on(id, cpu as u32) == 0
    }
}

fn await_exited(state: *mut State, slots: &[usize]) -> bool {
    let end = deadline(5);
    loop {
        if slots
            .iter()
            .all(|&slot| unsafe { abi::kcore_task_state(value(state, slot)) } == EXITED)
        {
            return true;
        }
        if unsafe { abi::kcore_sched_run() } != 0 || unsafe { abi::kcore_now() } >= end {
            return false;
        }
        core::hint::spin_loop();
    }
}

fn panic_case(state: *mut State, failed_cpu: usize) -> bool {
    reset(state);
    let healthy_cpu = 1 - failed_cpu;
    if !start(state, healthy, healthy_cpu, 6 + healthy_cpu) {
        return false;
    }
    let config = PanicConfig {
        words: unsafe { core::ptr::addr_of_mut!((*state).words).cast() },
        cpu: failed_cpu as u32,
    };
    let create_args = KcompCreateArgs {
        config_abi: CONFIG_ABI,
        config: (&config as *const PanicConfig).cast(),
        config_len: core::mem::size_of::<PanicConfig>(),
    };
    let mut failed = u32::MAX;
    let from = trace::cursor();
    let loaded =
        unsafe { abi::kcore_component_create(b"kcomp_smp".as_ptr(), 9, &create_args, &mut failed) }
            == 0;
    loaded
        && await_exited(state, &[6, 7])
        && trace::component_failed(from, failed)
        && value(state, 8) == 1 << healthy_cpu
        && value(state, 2 + healthy_cpu) == 128
}

pub fn group(checks: &mut Checks, state: *mut State, rr_id: i32) {
    if unsafe { abi::kcore_machine_cpu_count() } < 2 {
        kcomp_sdk::klog!("[core-test] SMP needs two CPUs; skipped");
        return;
    }
    checks.group("SMP component scheduling");
    reset(state);
    let started = start(state, ping_pong, 0, 6)
        && start(state, ping_pong, 1, 7)
        && start(state, helper, 0, 12)
        && start(state, helper, 1, 13);
    let exited = started && await_exited(state, &[6, 7, 12, 13]);
    checks.check(50, "smp-parallel", exited && value(state, 0) == 3);
    checks.check(
        51,
        "smp-remote-park-wake",
        exited && value(state, 2) == 128 && value(state, 3) == 128,
    );
    checks.check(
        52,
        "smp-local-rr",
        exited && value(state, 4) == 32 && value(state, 5) == 32,
    );
    checks.check(53, "smp-task-completion", exited && value(state, 8) == 15);
    // Never reset storage still reachable by a live task after a failing check.
    let ap = exited && panic_case(state, 1);
    checks.check(54, "smp-panic-ap", ap);
    let bsp = ap && panic_case(state, 0);
    checks.check(55, "smp-panic-bsp", bsp);
    checks.check(
        56,
        "smp-scheduler-live",
        rr_id >= 0
            && Endpoint::<SchedulerPolicy>::lookup(rr_id as u32, SCHEDULER_POLICY_NAME).is_ok(),
    );
}
