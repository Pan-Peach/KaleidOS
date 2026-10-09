//! Real public IPC integration: separate provider image and owned Server Task.
use super::report::Checks;
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::{Errno, abi, ipc, management, mem};
#[path = "../../../kcomp_echo/contract.rs"]
#[allow(dead_code)]
mod contract;
use contract::*;
struct Peer {
    endpoint: u64,
    byte: u8,
    result: AtomicU32,
}
struct State {
    endpoints: [u64; 3],
    result: AtomicU32,
    peers: [Peer; 2],
    held: [Peer; 4],
}
fn deadline() -> u64 {
    unsafe { abi::kcore_now() + abi::kcore_timebase_hz() * 10 }
}
fn start(entry: abi::KcompTaskEntry, arg: *mut ()) -> Option<u32> {
    let mut task = 0;
    if unsafe { abi::kcore_task_create(entry, arg, &mut task) } != 0
        || unsafe { abi::kcore_task_start(task) } != 0
    {
        None
    } else {
        Some(task)
    }
}
fn finish(task: u32) -> bool {
    let end = deadline();
    while unsafe { abi::kcore_task_state(task) } != 4 {
        if unsafe { abi::kcore_now() } >= end || management::run_tasks().is_err() {
            return false;
        }
    }
    true
}
fn ready_call(endpoint: u64, input: &[u8], output: &mut [u8]) -> kcomp_sdk::Result<usize> {
    let end = deadline();
    loop {
        match ipc::call(endpoint, input, output) {
            Err(Errno::ENOTCONN | Errno::EACCES) if unsafe { abi::kcore_now() } < end => {
                management::yield_task()?
            }
            result => return result,
        }
    }
}
extern "C" fn client(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let ep = state.endpoints[0];
    let mut result = 0;
    let input = [0x5a; 512];
    let mut output = [0; 512];
    if ready_call(ep, &input[..8], &mut output) == Ok(8) && output[..8] == input[..8] {
        result |= 1;
    }
    if ipc::listen(ep) == Err(Errno::EACCES)
        && ipc::grant(ep, management::current_component().unwrap()) == Ok(())
        && ipc::grant(ep, u32::MAX) == Err(Errno::ESRCH)
        && ipc::submit(u64::MAX, &[]) == Err(Errno::ENOENT)
    {
        let mut request = [FOREIGN_GRANT; 9];
        request[1..].copy_from_slice(&state.endpoints[1].to_le_bytes());
        if ipc::call(ep, &request, &mut []) == Ok(0) {
            result |= 2;
        }
    }
    let mut id = 0;
    if unsafe { abi::kcore_ipc_submit(ep, core::ptr::null(), 1, &mut id) } == Errno::EFAULT.code()
        && unsafe { abi::kcore_ipc_submit(ep, core::ptr::null(), ipc::MESSAGE_MAX + 1, &mut id) }
            == Errno::EMSGSIZE.code()
        && unsafe { abi::kcore_ipc_submit(ep, input.as_ptr(), 1, core::ptr::null_mut()) }
            == Errno::EFAULT.code()
    {
        result |= 4;
    }
    if ipc::call(ep, &[SELF_CALL], &mut []) == Ok(0) {
        result |= 8;
    }
    if ipc::service::invoke(ep, SERVICE_ECHO, &[], &input[..64], &mut output[..64]) == Ok(0)
        && output[..64] == input[..64]
    {
        result |= 256;
    }
    // Cancellation may win before receive or race a committed Echo reply.
    if let Ok(request) = ipc::submit(ep, &input[..1]) {
        let canceled = ipc::cancel(request);
        let completion = loop {
            match ipc::collect(request, &mut output) {
                Err(Errno::EAGAIN) => {
                    if ipc::wait_request(request).is_err() {
                        break Err(Errno::EIO);
                    }
                }
                value => break value,
            }
        };
        if (canceled == Ok(()) && completion == Err(Errno::ECANCELED))
            || (canceled == Err(Errno::EALREADY) && completion == Ok(1))
        {
            result |= 16;
        }
    }
    if ready_call(state.endpoints[1], &[], &mut output) == Ok(0)
        && ipc::call(state.endpoints[1], &[PANIC], &mut output) == Err(Errno::ENOTCONN)
        && ipc::submit(state.endpoints[1], &[]) == Err(Errno::ENOENT)
    {
        result |= 32;
    }
    if ready_call(state.endpoints[2], &[], &mut output) == Ok(0)
        && ipc::call(state.endpoints[2], &[EXIT], &mut output) == Err(Errno::ENOTCONN)
        && ipc::submit(state.endpoints[2], &[]) == Err(Errno::ENOENT)
    {
        result |= 64;
    }
    let hz = unsafe { abi::kcore_timebase_hz() };
    kcomp_sdk::klog!(
        "[ipc-baseline] transport=request-reply hz={} batches=31 operations_per_batch=32 trace=profile",
        hz
    );
    let mut measured = true;
    for size in [0, 8, 64, 512] {
        let mut samples = [0u64; 31];
        for batch in 0..35 {
            let begin = unsafe { abi::kcore_now() };
            for _ in 0..32 {
                if ipc::call(ep, &input[..size], &mut output) != Ok(size) {
                    measured = false;
                    break;
                }
            }
            let end = unsafe { abi::kcore_now() };
            if end < begin || output[..size] != input[..size] {
                measured = false;
            }
            if batch >= 4 {
                samples[batch - 4] = end.saturating_sub(begin);
            }
        }
        samples.sort_unstable();
        kcomp_sdk::klog!(
            "[ipc-baseline] transport=request-reply size={} min={} median={} p95={} max={}",
            size,
            samples[0],
            samples[15],
            samples[29],
            samples[30]
        );
    }
    if measured {
        result |= 128;
    }
    state.result.store(result, Ordering::Release);
    management::exit_task();
}
extern "C" fn peer(arg: *mut ()) {
    let peer = unsafe { &*arg.cast::<Peer>() };
    let mut output = [0; 1];
    let code = ipc::call(peer.endpoint, &[peer.byte], &mut output);
    peer.result.store(
        u32::from(code == Ok(1) && output == [peer.byte]),
        Ordering::Release,
    );
    management::exit_task();
}
extern "C" fn hold(arg: *mut ()) {
    let peer = unsafe { &*arg.cast::<Peer>() };
    let request = ipc::submit(peer.endpoint, &[peer.byte]);
    peer.result
        .store(if request.is_ok() { 2 } else { 3 }, Ordering::Release);
    if let Ok(request) = request {
        // Leave the result uncollected so it consumes one bounded Core slot.
        if unsafe { abi::kcore_task_park() } == 0 {
            let mut output = [0; 1];
            let result = loop {
                match ipc::collect(request, &mut output) {
                    Err(Errno::EAGAIN) => {
                        let _ = ipc::wait_request(request);
                    }
                    value => break value,
                }
            };
            peer.result.store(
                u32::from(result == Ok(1) && output == [peer.byte]),
                Ordering::Release,
            );
        }
    }
    management::exit_task();
}
extern "C" fn full(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let result = ipc::submit(state.endpoints[0], &[]);
    if let Ok(request) = result {
        let _ = ipc::cancel(request);
        let _ = ipc::collect(request, &mut [0; ipc::MESSAGE_MAX]);
    }
    state
        .result
        .store(u32::from(result == Err(Errno::ENOBUFS)), Ordering::Release);
    management::exit_task();
}
extern "C" fn abandon(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    state.result.store(
        u32::from(ipc::submit(state.endpoints[0], &[0x33]).is_ok()),
        Ordering::Release,
    );
    // Core retires this caller's request without writing to its abandoned stack.
    management::exit_task();
}
extern "C" fn stop(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let result = ipc::call(state.endpoints[0], &[STOP], &mut []);
    state
        .result
        .store(u32::from(result == Ok(0)), Ordering::Release);
    management::exit_task();
}
pub fn group(checks: &mut Checks) {
    checks.group("endpoint-request-reply");
    let owner = management::current_component().unwrap();
    let region = mem::mem_alloc(
        core::mem::size_of::<State>() as u64,
        core::mem::align_of::<State>() as u64,
    )
    .unwrap();
    let state = region.base as *mut State;
    unsafe {
        state.write(State {
            endpoints: [0; 3],
            result: AtomicU32::new(0),
            peers: [
                Peer {
                    endpoint: 0,
                    byte: 0x11,
                    result: AtomicU32::new(0),
                },
                Peer {
                    endpoint: 0,
                    byte: 0x22,
                    result: AtomicU32::new(0),
                },
            ],
            held: core::array::from_fn(|index| Peer {
                endpoint: 0,
                byte: index as u8,
                result: AtomicU32::new(0),
            }),
        })
    };
    let mut providers = [0; 3];
    for (index, provider) in providers.iter_mut().enumerate() {
        let cpu = if index == 0 {
            u32::from(cfg!(target_arch = "riscv64"))
        } else {
            0
        };
        let mut config = [0; 8];
        config[..4].copy_from_slice(&owner.to_le_bytes());
        config[4..].copy_from_slice(&cpu.to_le_bytes());
        let created = management::create(
            b"kcomp_echo",
            management::ExecutionDomain::KernelNative,
            CONFIG_ABI,
            &config,
        );
        let Ok(id) = created else {
            checks.check("echo-create", false);
            return;
        };
        *provider = id;
        let mut endpoint = 0;
        assert_eq!(
            unsafe {
                abi::kcore_endpoint_lookup(id, NAME.as_ptr(), NAME.len(), CONTRACT, &mut endpoint)
            },
            0
        );
        unsafe { (*state).endpoints[index] = endpoint };
    }
    let client_done = start(client, state.cast()).is_some_and(finish);
    let bits = unsafe { (*state).result.load(Ordering::Acquire) };
    for (name, mask) in [
        ("ipc-echo-owned-copy", 1),
        ("ipc-identity-permissions", 2),
        ("ipc-invalid-parameters", 4),
        ("ipc-self-wait-rejected", 8),
        ("ipc-cancel-terminal", 16),
        ("ipc-provider-panic", 32),
        ("ipc-server-exit", 64),
        ("ipc-real-roundtrip-baseline", 128),
        ("ipc-service-envelope", 256),
    ] {
        checks.check(name, client_done && bits & mask != 0);
    }
    let ep = unsafe { (*state).endpoints[0] };
    for peer in unsafe { &mut (*state).peers } {
        peer.endpoint = ep;
    }
    let first = start(peer, unsafe {
        core::ptr::addr_of_mut!((*state).peers[0]).cast()
    });
    let second = start(peer, unsafe {
        core::ptr::addr_of_mut!((*state).peers[1]).cast()
    });
    let first_done = first.is_some_and(finish);
    let second_done = second.is_some_and(finish);
    let done = first_done && second_done;
    checks.check(
        "ipc-two-caller-matching",
        done && unsafe {
            (*state)
                .peers
                .iter()
                .all(|p| p.result.load(Ordering::Acquire) == 1)
        },
    );
    let mut held_tasks = [None; 4];
    for (index, task) in held_tasks.iter_mut().enumerate() {
        let held = unsafe { &mut (*state).held[index] };
        held.endpoint = ep;
        *task = start(hold, core::ptr::from_mut(held).cast());
    }
    let end = deadline();
    let mut held_ready = false;
    while unsafe { abi::kcore_now() } < end {
        held_ready = held_tasks.iter().all(Option::is_some)
            && unsafe {
                (*state)
                    .held
                    .iter()
                    .all(|p| p.result.load(Ordering::Acquire) == 2)
            };
        if held_ready {
            break;
        }
        let _ = management::run_tasks();
    }
    let probe_done = held_ready && start(full, state.cast()).is_some_and(finish);
    checks.check(
        "ipc-bounded-full",
        probe_done && unsafe { (*state).result.load(Ordering::Acquire) == 1 },
    );
    let mut held_done = true;
    for task in held_tasks.into_iter().flatten() {
        let _ = unsafe { abi::kcore_task_unpark(task) };
        held_done &= finish(task);
    }
    checks.check(
        "ipc-full-drain",
        held_done
            && unsafe {
                (*state)
                    .held
                    .iter()
                    .all(|p| p.result.load(Ordering::Acquire) == 1)
            },
    );
    let mut abandon_done = true;
    let mut abandoned = true;
    // More than the global pool capacity: orphaned slots would exhaust it.
    for _ in 0..20 {
        let finished = start(abandon, state.cast()).is_some_and(finish);
        abandon_done &= finished;
        abandoned &= finished && unsafe { (*state).result.load(Ordering::Acquire) == 1 };
    }
    checks.check("ipc-caller-exit-reclaims", abandoned);
    let done = start(stop, state.cast()).is_some_and(finish);
    checks.check(
        "ipc-close-drain",
        done && unsafe { (*state).result.load(Ordering::Acquire) == 1 },
    );
    // The server has no Direct exports and no live Task after close.
    checks.check("ipc-provider-stop", {
        let end = deadline();
        loop {
            let status = unsafe { abi::kcore_component_stop(providers[0]) };
            if status == 0 {
                break true;
            }
            if status != Errno::EBUSY.code() || unsafe { abi::kcore_now() } >= end {
                break false;
            }
            let _ = management::run_tasks();
        }
    });
    if client_done && first_done && second_done && held_done && abandon_done && done {
        let _ = mem::mem_release(region);
    }
}
