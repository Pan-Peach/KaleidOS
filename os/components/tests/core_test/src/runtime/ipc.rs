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
    finish_with_budget(task, 10)
}
fn finish_with_budget(task: u32, seconds: u64) -> bool {
    let end = unsafe { abi::kcore_now() + abi::kcore_timebase_hz() * seconds };
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
    if kcomp_sdk::generated::echo_wire::echo(ep, &input[..64], &mut output[..64]) == Ok(())
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

struct DomainCall {
    endpoint: u64,
    forward: bool,
    stop: bool,
    result: AtomicU32,
}
extern "C" fn domain_client(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<DomainCall>() };
    let mut output = [0; 8];
    let payload = if state.stop {
        &[STOP][..]
    } else if state.forward {
        &[FORWARD, 1, 2, 3][..]
    } else {
        &[1, 2, 3][..]
    };
    let passed = if state.stop {
        ready_call(state.endpoint, payload, &mut output) == Ok(0)
    } else {
        ready_call(state.endpoint, payload, &mut output) == Ok(3) && output[..3] == [1, 2, 3]
    };
    state.result.store(u32::from(passed), Ordering::Release);
    management::exit_task();
}
fn domain_provider(
    domain: management::ExecutionDomain,
    owner: u32,
    target: u64,
) -> Option<(u32, u64)> {
    let mut config = [0; 16];
    config[..4].copy_from_slice(&owner.to_le_bytes());
    config[8..].copy_from_slice(&target.to_le_bytes());
    let id = match management::create(b"kcomp_echo", domain, CONFIG_ABI, &config) {
        Ok(id) => id,
        Err(e) => {
            kcomp_sdk::klog!("domain echo create {:?}: {:?}", domain, e);
            return None;
        }
    };
    let mut ep = 0;
    if unsafe { abi::kcore_endpoint_lookup(id, NAME.as_ptr(), NAME.len(), CONTRACT, &mut ep) } != 0
    {
        return None;
    }
    Some((id, ep))
}
fn private_available(checks: &mut Checks) -> bool {
    // The public load API checks platform capability before looking up an
    // artifact. A missing name creates no instance and needs no CoreTest cfg
    // mirror of the resolved platform configuration.
    match management::load(
        b"__runtime_capability_probe_missing",
        management::ExecutionDomain::IsolatedNative,
    ) {
        Err(Errno::ENOENT) => true,
        Err(Errno::ENOTSUP) => {
            kcomp_sdk::klog!(
                "private-domain tests not applicable: Core reports unsupported deployment"
            );
            false
        }
        _ => {
            checks.check("private-domain-capability-probe", false);
            false
        }
    }
}
pub fn domain_group(checks: &mut Checks) {
    if !private_available(checks) {
        return;
    }
    use management::ExecutionDomain::{
        IsolatedNative as I, KernelNative as K, SandboxedNative as U,
    };
    let owner = management::current_component().unwrap();
    let pairs = [
        ("ipc-K-I-I-K", I, K),
        ("ipc-K-I-I-I", I, I),
        ("ipc-K-U-U-K", U, K),
        ("ipc-K-U-U-I", U, I),
        ("ipc-K-I-I-U", I, U),
        ("ipc-K-U-U-U", U, U),
    ];
    for (name, relay_domain, target_domain) in pairs {
        if !cfg!(target_arch = "riscv64") && (relay_domain == U || target_domain == U) {
            continue;
        }
        let Some((target_id, target)) = domain_provider(target_domain, owner, 0) else {
            checks.check(name, false);
            continue;
        };
        let Some((relay_id, relay)) = domain_provider(relay_domain, owner, target) else {
            checks.check(name, false);
            continue;
        };
        let region = mem::mem_alloc(
            core::mem::size_of::<DomainCall>() as u64,
            core::mem::align_of::<DomainCall>() as u64,
        )
        .unwrap();
        let pointer = region.base as *mut DomainCall;
        unsafe {
            pointer.write(DomainCall {
                endpoint: target,
                forward: false,
                stop: false,
                result: AtomicU32::new(0),
            });
        }
        // Each argument remains owned until its actual Task exits. A timeout
        // retains backing and must not mutate memory still borrowed remotely.
        if !start(domain_client, pointer.cast()).is_some_and(finish) {
            checks.check(name, false);
            continue;
        }
        let ready = unsafe { (*pointer).result.load(Ordering::Acquire) } == 1;
        let grant = unsafe { abi::kcore_ipc_grant(target, relay_id) } == 0;
        unsafe {
            (*pointer).endpoint = relay;
            (*pointer).forward = true;
            (*pointer).result.store(0, Ordering::Release);
        }
        let done = ready && grant && start(domain_client, pointer.cast()).is_some_and(finish);
        kcomp_sdk::klog!(
            "domain pair {} ready={} grant={} done={} result={}",
            name,
            ready,
            grant,
            done,
            unsafe { (*pointer).result.load(Ordering::Acquire) }
        );
        checks.check(
            name,
            done && unsafe { (*pointer).result.load(Ordering::Acquire) } == 1,
        );
        if ready && grant && !done {
            continue;
        }
        let mut cleanup_done = true;
        for (id, ep) in [(relay_id, relay), (target_id, target)] {
            unsafe {
                (*pointer).endpoint = ep;
                (*pointer).stop = true;
            }
            if !start(domain_client, pointer.cast()).is_some_and(finish) {
                cleanup_done = false;
                break;
            }
            let _ = unsafe { abi::kcore_component_stop(id) };
            if target_domain != K || id == relay_id {
                let _ = unsafe { abi::kcore_component_reclaim(id) };
            }
        }
        if cleanup_done {
            let _ = mem::mem_release(region);
        }
    }
}

struct LifecycleResult {
    result: AtomicU32,
}

fn runtime_stats() -> Option<abi::RuntimeStatsAbi> {
    let mut value = core::mem::MaybeUninit::uninit();
    (unsafe { abi::kcore_runtime_stats(value.as_mut_ptr()) } == 0)
        .then(|| unsafe { value.assume_init() })
}

fn metadata_pages(stats: &abi::RuntimeStatsAbi) -> i64 {
    i64::from(stats.component_metadata_pages)
        + i64::from(stats.endpoint_metadata_pages)
        + i64::from(stats.space_metadata_pages)
        + i64::from(stats.mapping_metadata_pages)
        + i64::from(stats.slab_pages)
}

fn same_reclaimable_resources(before: &abi::RuntimeStatsAbi, after: &abi::RuntimeStatsAbi) -> bool {
    before.address_spaces == after.address_spaces
        && before.private_mapping_pages == after.private_mapping_pages
        && before.page_table_pages == after.page_table_pages
        && before.tasks == after.tasks
        && before.task_stack_pages == after.task_stack_pages
        && before.ipc_servers == after.ipc_servers
        && before.ipc_requests == after.ipc_requests
        && before.exclusions == after.exclusions
}

fn log_runtime_stats(round: u32, unaccounted: i64, stats: &abi::RuntimeStatsAbi) {
    kcomp_sdk::klog!(
        "[runtime-accounting] round={} free={} slab={}/{}/{} comp={}/{} comp_pages={} ep={}/{} ep_pages={} meta_slab={}/{} unaccounted={}",
        round,
        stats.free_pages,
        stats.slab_pages,
        stats.slab_objects,
        stats.slab_bytes,
        stats.component_records,
        stats.reclaimed_components,
        stats.component_metadata_pages,
        stats.endpoint_records,
        stats.endpoint_names,
        stats.endpoint_metadata_pages,
        stats.metadata_slab_objects,
        stats.metadata_slab_bytes,
        unaccounted
    );
    kcomp_sdk::klog!(
        "[runtime-resources] round={} as={} mapped={} tables={} as_meta={} tasks={} stacks={} servers={} requests={} exclusions={} plan_meta={}",
        round,
        stats.address_spaces,
        stats.private_mapping_pages,
        stats.page_table_pages,
        stats.space_metadata_pages,
        stats.tasks,
        stats.task_stack_pages,
        stats.ipc_servers,
        stats.ipc_requests,
        stats.exclusions,
        stats.mapping_metadata_pages
    );
}

extern "C" fn lifecycle_client(arg: *mut ()) {
    use management::ExecutionDomain::{IsolatedNative as I, SandboxedNative as U};
    let result = unsafe { &*arg.cast::<LifecycleResult>() };
    let owner = management::current_component().unwrap();
    let mut passed = true;
    let mut returned = 0u64;
    let mut retained_peak = 0u32;
    let initial = unsafe { abi::kcore_free_page_count() };
    let task_count = unsafe { abi::kcore_task_count() };
    let Some(baseline) = runtime_stats() else {
        management::exit_task();
    };
    // The two intentional small allocations per tombstone are the artifact
    // and endpoint names. Slab slot size follows the existing word minimum.
    let name_bytes = (b"kcomp_echo"
        .len()
        .max(core::mem::size_of::<usize>())
        .next_power_of_two()
        + NAME
            .len()
            .max(core::mem::size_of::<usize>())
            .next_power_of_two()) as u32;
    log_runtime_stats(0, 0, &baseline);
    for round in 0..1000 {
        let domain = if cfg!(target_arch = "riscv64") && round % 2 == 1 {
            U
        } else {
            I
        };
        let Some((id, ep)) = domain_provider(domain, owner, 0) else {
            passed = false;
            break;
        };
        let mut output = [0; 8];
        let before_probe = unsafe { abi::kcore_task_count() };
        if ready_call(ep, &[INVALID_BUFFER], &mut output) != Ok(0)
            || unsafe { abi::kcore_task_count() } != before_probe
        {
            passed = false;
            break;
        }
        let payload = [1, 2, 3, 4, 5, 6, 7, 8];
        if kcomp_sdk::generated::echo_wire::echo(ep, &payload, &mut output) != Ok(())
            || output != payload
        {
            passed = false;
            break;
        }
        let before = unsafe { abi::kcore_free_page_count() };
        if round % 10 == 1 || round % 10 == 2 {
            // U attempts a supervisor-page load; I takes an illegal instruction.
            // Both must return through the actual trap before task teardown.
            let fault = if domain == U { MEMORY_FAULT } else { FAULT };
            if ipc::call(ep, &[fault], &mut []) != Err(Errno::ENOTCONN) {
                passed = false;
                break;
            }
            let end = deadline();
            loop {
                let status = unsafe { abi::kcore_component_force_stop(id) };
                if status == 0 {
                    break;
                }
                if status != Errno::EBUSY.code()
                    || unsafe { abi::kcore_now() } >= end
                    || management::yield_task().is_err()
                {
                    passed = false;
                    break;
                }
            }
        } else if round % 10 == 3 && domain == U {
            // An unyielding U loop must return through the hardware timer.
            let Ok(request) = ipc::submit(ep, &[BUSY]) else {
                passed = false;
                break;
            };
            let _ = management::yield_task();
            let stopped = unsafe { abi::kcore_component_force_stop(id) };
            if stopped != 0 || ipc::collect(request, &mut []) != Err(Errno::ENOTCONN) {
                passed = false;
                break;
            }
        } else {
            let request = if round % 10 == 4 || round % 10 == 5 {
                // Accepted work yields until notification; queued work has not
                // yet run when stop commits. Both must retain a successful reply.
                let payload = if round % 10 == 4 { GRACEFUL } else { 42 };
                let Ok(request) = ipc::submit(ep, &[payload]) else {
                    passed = false;
                    break;
                };
                if round % 10 == 4 {
                    let _ = management::yield_task();
                }
                Some((request, payload))
            } else {
                None
            };
            let first = unsafe { abi::kcore_component_stop(id) };
            if first != Errno::EBUSY.code() || ipc::submit(ep, &[]) != Err(Errno::ENOENT) {
                passed = false;
                break;
            }
            let end = deadline();
            loop {
                let status = unsafe { abi::kcore_component_stop(id) };
                if status == 0 {
                    break;
                }
                if status != Errno::EBUSY.code()
                    || unsafe { abi::kcore_now() } >= end
                    || management::yield_task().is_err()
                {
                    passed = false;
                    break;
                }
            }
            if let Some((request, payload)) = request {
                let mut output = [0; 8];
                if ipc::collect(request, &mut output) != Ok(1) || output[0] != payload {
                    passed = false;
                    break;
                }
            }
            if unsafe { abi::kcore_component_stop(id) } != 0 {
                passed = false;
                break;
            }
        }
        let reclaimed = unsafe { abi::kcore_component_reclaim(id) };
        let after = unsafe { abi::kcore_free_page_count() };
        let Some(stats) = runtime_stats() else {
            passed = false;
            break;
        };
        let unaccounted = i64::from(baseline.free_pages)
            - i64::from(stats.free_pages)
            - (metadata_pages(&stats) - metadata_pages(&baseline));
        if reclaimed != 0
            || unsafe { abi::kcore_component_reclaim(id) } != 0
            || ipc::submit(ep, &[]) != Err(Errno::ENOENT)
            || unsafe { abi::kcore_task_count() } != task_count
            || after <= before
            || !same_reclaimable_resources(&baseline, &stats)
            || stats.component_records != baseline.component_records + round + 1
            || stats.reclaimed_components != baseline.reclaimed_components + round + 1
            || stats.endpoint_records != baseline.endpoint_records + round + 1
            || stats.endpoint_names != baseline.endpoint_names + round + 1
            || i64::from(stats.slab_objects) - i64::from(baseline.slab_objects)
                != 2 * i64::from(round + 1) + i64::from(stats.metadata_slab_objects)
                    - i64::from(baseline.metadata_slab_objects)
            || i64::from(stats.slab_bytes) - i64::from(baseline.slab_bytes)
                != i64::from(name_bytes * (round + 1)) + i64::from(stats.metadata_slab_bytes)
                    - i64::from(baseline.metadata_slab_bytes)
            || unaccounted != 0
        {
            log_runtime_stats(round + 1, unaccounted, &stats);
            passed = false;
            break;
        }
        returned += u64::from(after - before);
        retained_peak = retained_peak.max(initial.saturating_sub(after));
        if round % 100 == 99 {
            kcomp_sdk::klog!(
                "[runtime-stress] rounds={} tasks={} free={} returned_pages={} retained_peak={}",
                round + 1,
                task_count,
                after,
                returned,
                retained_peak
            );
            log_runtime_stats(round + 1, unaccounted, &stats);
        }
    }
    if passed && cfg!(target_arch = "riscv64") {
        let mut config = [0; 16];
        config[..4].copy_from_slice(&owner.to_le_bytes());
        config[4..8].copy_from_slice(&1u32.to_le_bytes());
        passed = management::create(b"kcomp_echo", U, CONFIG_ABI, &config).is_ok_and(|id| {
            let mut ep = 0;
            if unsafe {
                abi::kcore_endpoint_lookup(id, NAME.as_ptr(), NAME.len(), CONTRACT, &mut ep)
            } != 0
            {
                return false;
            }
            if ready_call(ep, &[BUSY_ACK], &mut []) != Ok(0) {
                return false;
            }
            let now = unsafe { abi::kcore_now() };
            while unsafe { abi::kcore_now() } < now + unsafe { abi::kcore_timebase_hz() } / 1000 {
                core::hint::spin_loop();
            }
            let end = deadline();
            let first = unsafe { abi::kcore_component_force_stop(id) };
            let mut stop = first;
            while stop == Errno::EBUSY.code() && unsafe { abi::kcore_now() } < end {
                stop = unsafe { abi::kcore_component_force_stop(id) };
            }
            let reclaimed = unsafe { abi::kcore_component_reclaim(id) };
            kcomp_sdk::klog!(
                "[runtime-smp] U busy AP first={} stop={} reclaim={}",
                first,
                stop,
                reclaimed
            );
            stop == 0 && reclaimed == 0 && ipc::submit(ep, &[]) == Err(Errno::ENOENT)
        });
    }
    result.result.store(u32::from(passed), Ordering::Release);
    management::exit_task();
}
pub fn lifecycle_group(checks: &mut Checks) {
    if !private_available(checks) {
        return;
    }
    let region = mem::mem_alloc(
        core::mem::size_of::<LifecycleResult>() as u64,
        core::mem::align_of::<LifecycleResult>() as u64,
    )
    .unwrap();
    let result = region.base as *mut LifecycleResult;
    unsafe {
        result.write(LifecycleResult {
            result: AtomicU32::new(0),
        });
    }
    let done =
        start(lifecycle_client, result.cast()).is_some_and(|task| finish_with_budget(task, 240));
    checks.check(
        "component-private-reclaim-1000",
        done && unsafe { (*result).result.load(Ordering::Acquire) } == 1,
    );
    // A timeout may leave execution pending on another CPU. Keep the argument
    // backing until confirmed exit; never lend a short-lived create-stack local.
    if done {
        let _ = mem::mem_release(region);
    }
}

fn stop_until(id: u32, expected: i32) -> bool {
    let end = deadline();
    loop {
        let code = unsafe { abi::kcore_component_stop(id) };
        if code == expected {
            return true;
        }
        if code != Errno::EBUSY.code() || unsafe { abi::kcore_now() } >= end {
            return false;
        }
        if management::yield_task().is_err() {
            return false;
        }
    }
}
fn force_until(id: u32) -> bool {
    let end = deadline();
    loop {
        let code = unsafe { abi::kcore_component_force_stop(id) };
        if code == 0 {
            return true;
        }
        if code != Errno::EBUSY.code() || unsafe { abi::kcore_now() } >= end {
            return false;
        }
        if management::yield_task().is_err() {
            return false;
        }
    }
}
extern "C" fn graceful_client(arg: *mut ()) {
    use management::ExecutionDomain::{
        IsolatedNative as I, KernelNative as K, SandboxedNative as U,
    };
    let result = unsafe { &*arg.cast::<LifecycleResult>() };
    let owner = management::current_component().unwrap();
    let private = management::load(b"__runtime_capability_probe_missing", I) == Err(Errno::ENOENT);
    let mut passed = true;
    for domain in [K, I, U] {
        if (domain != K && !private) || (domain == U && cfg!(target_arch = "riscv32")) {
            continue;
        }
        let cpus: u32 = if private && cfg!(target_arch = "riscv64") {
            2
        } else {
            1
        };
        for cpu in 0..cpus {
            for mode in 0u32..=if domain == U { 3 } else { 2 } {
                let mut config = [0; 24];
                config[..4].copy_from_slice(&owner.to_le_bytes());
                config[4..8].copy_from_slice(&cpu.to_le_bytes());
                config[16..20].copy_from_slice(&mode.to_le_bytes());
                config[20..24].copy_from_slice(&2u32.to_le_bytes());
                let Ok(id) = management::create(b"kcomp_echo", domain, CONFIG_ABI, &config) else {
                    passed = false;
                    break;
                };
                let mut ep = 0;
                if unsafe {
                    abi::kcore_endpoint_lookup(id, NAME.as_ptr(), NAME.len(), CONTRACT, &mut ep)
                } != 0
                    || ready_call(ep, &[42], &mut [0; 8]) != Ok(1)
                {
                    passed = false;
                    break;
                }
                let Ok(request) = ipc::submit(ep, &[GRACEFUL]) else {
                    passed = false;
                    break;
                };
                let _ = management::yield_task();
                let expected = if mode == 0 { 0 } else { Errno::EIO.code() };
                if !stop_until(id, expected)
                    || ipc::collect(request, &mut [0; 8]) != Ok(1)
                    || super::component_state(id) != Some(if mode == 0 { 5 } else { 6 })
                    || unsafe { abi::kcore_component_stop(id) }
                        != if mode == 0 { 0 } else { Errno::EINVAL.code() }
                    || ipc::submit(ep, &[]) != Err(Errno::ENOENT)
                {
                    passed = false;
                    break;
                }
                if domain != K
                    && (!force_until(id) || unsafe { abi::kcore_component_reclaim(id) } != 0)
                {
                    passed = false;
                    break;
                }
            }
            if !passed {
                break;
            }
        }
        if !passed {
            break;
        }
        // Cooperative code which ignores stop remains Stopping for a finite
        // observation window; explicit Force cancels its saved execution.
        let Some((id, ep)) = domain_provider(domain, owner, 0) else {
            passed = false;
            break;
        };
        if ready_call(ep, &[IGNORE_STOP], &mut []) != Ok(0) {
            passed = false;
            break;
        }
        let end = unsafe { abi::kcore_now() + abi::kcore_timebase_hz() / 50 };
        let mut attempts = 0;
        while unsafe { abi::kcore_now() } < end {
            if unsafe { abi::kcore_component_stop(id) } != Errno::EBUSY.code() {
                passed = false;
                break;
            }
            attempts += 1;
            let _ = management::yield_task();
        }
        if attempts == 0
            || !passed
            || super::component_state(id) != Some(4)
            || !force_until(id)
            || (domain != K && unsafe { abi::kcore_component_reclaim(id) } != 0)
        {
            passed = false;
            break;
        }
        kcomp_sdk::klog!(
            "[graceful] domain={:?} multi-task/destroy/timeout: PASS",
            domain
        );
    }
    result.result.store(u32::from(passed), Ordering::Release);
    management::exit_task();
}
pub fn graceful_group(checks: &mut Checks) {
    let region = mem::mem_alloc(
        core::mem::size_of::<LifecycleResult>() as u64,
        core::mem::align_of::<LifecycleResult>() as u64,
    )
    .unwrap();
    let result = region.base as *mut LifecycleResult;
    unsafe {
        result.write(LifecycleResult {
            result: AtomicU32::new(0),
        });
    }
    let done =
        start(graceful_client, result.cast()).is_some_and(|task| finish_with_budget(task, 60));
    checks.check(
        "component-graceful-drain",
        done && unsafe { (*result).result.load(Ordering::Acquire) } == 1,
    );
    if done {
        let _ = mem::mem_release(region);
    }
}
