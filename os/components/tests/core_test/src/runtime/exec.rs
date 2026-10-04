//! Integration tests use only real component/Core C APIs and ordinary ELF images.
use super::report::Checks;
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::{abi, endpoint::Endpoint, management, posix};

const ZERO: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/exit-zero");
const SEVEN: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/exit-seven");
const WRITE: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/write");
const STACK: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/stack-bss");
const BAD: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/bad-pointer");
const PRIVILEGED: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/privileged");
const TEXT: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/text-write");
const CORE_READ: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/core-read");
const STACK_EXECUTE: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/stack-execute");
const BREAKPOINT: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/breakpoint");
const PROTECT: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/protect");
const FORK: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/fork-exec");
const TARGET: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/target");
const TIMER: &[u8] = include_bytes!("../../../../../../build/exec-fixtures/timer");

fn family(images: &[(&[u8], &[u8])], argv: &[&[u8]]) -> kcomp_sdk::Result<posix::ProcessBinding> {
    let config = posix::encode(images, argv, &[])?;
    let id = management::create(b"posix", posix::KCOMP_POSIX_CREATE_CONFIG_ABI, &config)?;
    Endpoint::<posix::PosixProcess>::lookup(id, posix::KCOMP_POSIX_PROCESS_NAME)?.bind()
}
fn wait(binding: &posix::ProcessBinding) -> u32 {
    let start = unsafe { abi::kcore_now() };
    let timeout = unsafe { abi::kcore_timebase_hz() } * 20;
    loop {
        let status = binding.status().expect("process status");
        if status.exited && status.live == 0 {
            return status.wait_status;
        }
        assert!(
            unsafe { abi::kcore_now() } - start < timeout,
            "user process timed out"
        );
        management::yield_task().expect("yield while waiting");
    }
}
extern "C" fn suite(arg: *mut ()) {
    let result = unsafe { &*arg.cast::<AtomicU32>() };
    for (name, image, expected, args) in [
        ("exit-zero", ZERO, 0, &[b"/main".as_slice()][..]),
        ("exit-seven", SEVEN, 7 << 8, &[b"/main".as_slice()][..]),
        ("write", WRITE, 0, &[b"/main".as_slice()][..]),
        (
            "stack-bss",
            STACK,
            0,
            &[b"/main".as_slice(), b"probe".as_slice()][..],
        ),
        ("bad-pointer", BAD, 0, &[b"/main".as_slice()][..]),
        ("privileged", PRIVILEGED, 4, &[b"/main".as_slice()][..]),
        ("core-read", CORE_READ, 11, &[b"/main".as_slice()][..]),
        (
            "stack-execute",
            STACK_EXECUTE,
            11,
            &[b"/main".as_slice()][..],
        ),
        ("breakpoint", BREAKPOINT, 5, &[b"/main".as_slice()][..]),
        ("protect", PROTECT, 11, &[b"/main".as_slice()][..]),
        ("text-write", TEXT, 11, &[b"/main".as_slice()][..]),
    ] {
        let binding = family(&[(b"/main", image)], args).expect("spawn user ELF");
        assert_eq!(wait(&binding), expected, "unexpected user result: {name}");
        kcomp_sdk::klog!("[user-probe] {name}: PASS");
    }
    result.fetch_or(1, Ordering::Release);
    let binding = family(
        &[
            (b"/fork", FORK),
            (b"/target", TARGET),
            (b"/broken", b"not an ELF"),
        ],
        &[b"/fork"],
    )
    .expect("spawn fork/exec family");
    assert_eq!(wait(&binding), 0, "fork/exec/rollback/wait failed");
    kcomp_sdk::klog!("[user-probe] fork-exec-wait: PASS");
    result.fetch_or(2, Ordering::Release);
    let binding = family(&[(b"/timer", TIMER)], &[b"/timer"]).expect("spawn no-ecall loop");
    management::yield_task().expect("schedule userspace");
    assert!(
        !binding.status().unwrap().exited,
        "timer probe completed before another task got CPU"
    );
    kcomp_sdk::klog!("[user-probe] timer-service-alive: PASS");
    assert_eq!(wait(&binding), 0);
    kcomp_sdk::klog!("[user-probe] timer-return: PASS");
    kcomp_sdk::klog!("[user-probe] all: PASS");
    result.fetch_or(4, Ordering::Release);
    management::exit_task();
}
pub fn group(checks: &mut Checks) {
    checks.group("user execution");
    // The anchor stack stays live until sched_run returns; suite exits before
    // this state leaves scope. The task receives no Core-private state.
    let result = AtomicU32::new(0);
    let mut task = 0;
    let created =
        unsafe { abi::kcore_task_create(suite, &result as *const _ as *mut (), &mut task) } == 0;
    let started = created && unsafe { abi::kcore_task_start(task) } == 0;
    if started {
        super::schedule();
    }
    let bits = result.load(Ordering::Acquire);
    checks.check(57, "exec-elf", bits & 1 != 0);
    checks.check(58, "exec-fork-exec-wait", bits & 2 != 0);
    checks.check(59, "exec-timer", bits & 4 != 0);
}
