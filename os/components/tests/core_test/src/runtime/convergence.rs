//! Native composition experiment; requests/replies remain test Runtime state.
use super::report::Checks;
#[cfg(target_arch = "riscv64")]
use core::sync::atomic::AtomicI32;
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::{Errno, abi, management};

#[path = "../../../kcomp_checksum/contract.rs"]
#[allow(dead_code)]
mod contract;
use contract::*;

pub struct State {
    providers: [u32; 4],
    result: u32,
    #[cfg(target_arch = "riscv64")]
    endpoint: u64,
    #[cfg(target_arch = "riscv64")]
    control: [AtomicU32; 2],
    #[cfg(target_arch = "riscv64")]
    transport: AtomicI32,
}

fn create(mode: u32) -> Option<u32> {
    let cpu = u32::from(cfg!(target_arch = "riscv64"));
    let mut config = [0; 8];
    config[..4].copy_from_slice(&mode.to_le_bytes());
    config[4..].copy_from_slice(&cpu.to_le_bytes());
    management::create(
        b"kcomp_checksum",
        management::ExecutionDomain::KernelNative,
        CONFIG_ABI,
        &config,
    )
    .ok()
}

fn lookup(provider: u32) -> Option<u64> {
    let mut endpoint = 0;
    let code = unsafe {
        abi::kcore_endpoint_lookup(provider, NAME.as_ptr(), NAME.len(), CONTRACT, &mut endpoint)
    };
    (code == 0).then_some(endpoint)
}

struct Binding {
    api: &'static Api,
    ctx: *mut (),
}

fn bind(provider: u32) -> Option<Binding> {
    let endpoint = lookup(provider)?;
    let (mut mechanism, mut api, mut ctx) = (u32::MAX, 0, 0);
    let code = unsafe {
        abi::kcore_endpoint_bind(endpoint, CONTRACT, ABI, &mut mechanism, &mut api, &mut ctx)
    };
    if code != 0 || mechanism != abi::KCORE_ENDPOINT_MECHANISM_DIRECT || api == 0 || ctx == 0 {
        return None;
    }
    Some(Binding {
        api: unsafe { &*(api as *const Api) },
        ctx: ctx as *mut (),
    })
}

fn word(ptr: *mut u32) -> &'static AtomicU32 {
    // The Native provider's published mailbox is pinned for the boot lifetime.
    // Only atomic accesses touch these shared C-layout words.
    unsafe { AtomicU32::from_ptr(ptr) }
}

fn deadline() -> u64 {
    unsafe { abi::kcore_now().saturating_add(abi::kcore_timebase_hz() * 5) }
}

fn runtime_requests(binding: &Binding, hybrid: bool) -> bool {
    let mailbox = (binding.api.mailbox)(binding.ctx);
    let phase = word(unsafe { core::ptr::addr_of_mut!((*mailbox).phase) });
    let value = word(unsafe { core::ptr::addr_of_mut!((*mailbox).value) });
    let result = word(unsafe { core::ptr::addr_of_mut!((*mailbox).result) });
    let end = deadline();
    for n in 0..64 {
        // Reserve before writing; a second proposal must not overwrite input.
        if phase
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        value.store(0x0102_0300 + n, Ordering::Relaxed);
        if phase
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return false;
        }
        phase.store(2, Ordering::Release);
        if hybrid && (binding.api.checksum)(binding.ctx, 0x1020_3040) != 160 {
            return false;
        }
        while phase.load(Ordering::Acquire) != 3 {
            if unsafe { abi::kcore_now() } >= end || management::yield_task().is_err() {
                return false;
            }
        }
        if result.load(Ordering::Relaxed) != 6 + n {
            return false;
        }
        phase.store(0, Ordering::Release);
    }
    word(unsafe { core::ptr::addr_of_mut!((*mailbox).worker_calls) }).load(Ordering::Acquire) == 64
        && word(unsafe { core::ptr::addr_of_mut!((*mailbox).direct_calls) }).load(Ordering::Acquire)
            == if hybrid { 64 } else { 0 }
}

// Informational target baseline, outside trace assertion windows. Gate is the
// real Core entry; this does not measure a future Request/Reply implementation.
fn transport_baseline(provider: u32, binding: &Binding) -> bool {
    let Some(endpoint) = lookup(provider) else {
        return false;
    };
    let input = [0x5a; ECHO_MAX];
    let mut output = [0; ECHO_MAX];
    let mut stats = abi::TraceStatsAbi {
        capacity: 0,
        oldest_seq: 0,
        next_seq: 0,
        overwritten_total: 0,
        enabled_mask: 0,
    };
    let trace = unsafe { abi::kcore_trace_stats(&mut stats) } == 0;
    kcomp_sdk::klog!(
        "[ipc-baseline] unit=timebase-ticks hz={} trace_mask={} trace_known={} batches=31 operations_per_batch=32",
        unsafe { abi::kcore_timebase_hz() },
        stats.enabled_mask,
        trace
    );
    for size in [0, 8, 64, ECHO_MAX] {
        for gate in [false, true] {
            let mut samples = [0u64; 31];
            // Warm up four batches, then retain every measured batch.
            for batch in 0..35 {
                let begin = unsafe { abi::kcore_now() };
                for _ in 0..32 {
                    let result = if gate {
                        let mut method = i32::MIN;
                        let transport = unsafe {
                            abi::kcore_endpoint_call(
                                endpoint,
                                ECHO,
                                core::ptr::null(),
                                0,
                                input.as_ptr(),
                                size,
                                output.as_mut_ptr(),
                                size,
                                &mut method,
                            )
                        };
                        if transport != 0 { transport } else { method }
                    } else {
                        unsafe {
                            (binding.api.echo)(
                                binding.ctx,
                                input.as_ptr(),
                                output.as_mut_ptr(),
                                size,
                            )
                        }
                    };
                    if core::hint::black_box(result) != 0 {
                        return false;
                    }
                }
                let end = unsafe { abi::kcore_now() };
                if end < begin || output[..size] != input[..size] {
                    return false;
                }
                if batch >= 4 {
                    samples[batch - 4] = end - begin;
                }
            }
            samples.sort_unstable();
            kcomp_sdk::klog!(
                "[ipc-baseline] transport={} size={} min={} median={} p95={} max={}",
                if gate { "gate" } else { "direct" },
                size,
                samples[0],
                samples[15],
                samples[29],
                samples[30]
            );
        }
    }
    true
}

fn exercise(state: &State) -> Option<u32> {
    let a = bind(state.providers[0])?;
    let b = bind(state.providers[1])?;
    let active = bind(state.providers[2])?;
    let hybrid = bind(state.providers[3])?;
    let mut result = 0;
    let ma = (a.api.mailbox)(a.ctx);
    let mb = (b.api.mailbox)(b.ctx);
    let count_a = word(unsafe { core::ptr::addr_of_mut!((*ma).direct_calls) });
    let count_b = word(unsafe { core::ptr::addr_of_mut!((*mb).direct_calls) });
    if a.ctx != b.ctx
        && ma != mb
        && (a.api.worker)(a.ctx) == u32::MAX
        && (b.api.worker)(b.ctx) == u32::MAX
        && (a.api.checksum)(a.ctx, 0x1234_5678) == 276
        && count_a.load(Ordering::Acquire) == 1
        && count_b.load(Ordering::Acquire) == 0
    {
        result |= 1;
    }
    if unsafe { abi::kcore_component_stop(state.providers[0]) } == Errno::EBUSY.code()
        && (a.api.checksum)(a.ctx, 0xff) == 255
    {
        result |= 2;
    }
    if runtime_requests(&active, false) {
        result |= 4;
    }
    if runtime_requests(&hybrid, true) {
        result |= 8;
    }
    let worker = (hybrid.api.worker)(hybrid.ctx);
    if worker != u32::MAX && unsafe { abi::kcore_task_unpark(worker) } == Errno::EACCES.code() {
        result |= 16;
    }
    // A real consumer Task enters B's Init and Exit stacks. Each forbidden
    // switch must leave B's publication/lifecycle identity intact.
    if let Some(provider) = create(LIFECYCLE_PROBE)
        && lookup(provider).is_some()
        && unsafe { abi::kcore_component_stop(provider) } == 0
    {
        result |= 32;
    }
    if transport_baseline(state.providers[0], &a) {
        result |= 64;
    }
    Some(result)
}

extern "C" fn consumer(arg: *mut ()) {
    let state = arg.cast::<State>();
    let result = exercise(unsafe { &*state }).unwrap_or(0);
    // Stop the two provider-owned workers, including error paths. Their ctx is
    // retained; stopping a worker does not destroy the Component instance.
    for index in 2..4 {
        if let Some(binding) = bind(unsafe { (*state).providers[index] }) {
            let mailbox = (binding.api.mailbox)(binding.ctx);
            word(unsafe { core::ptr::addr_of_mut!((*mailbox).stop) }).store(1, Ordering::Release);
        }
    }
    unsafe { core::ptr::addr_of_mut!((*state).result).write(result) };
    management::exit_task();
}

fn start(entry: abi::KcompTaskEntry, state: *mut State, cpu: u32) -> Option<u32> {
    let mut task = 0;
    if unsafe { abi::kcore_task_create(entry, state.cast(), &mut task) } != 0
        || unsafe { abi::kcore_task_start_on(task, cpu) } != 0
    {
        return None;
    }
    Some(task)
}

fn await_exit(task: u32) -> bool {
    let end = deadline();
    while unsafe { abi::kcore_task_state(task) } != 4 {
        if unsafe { abi::kcore_now() } >= end || management::run_tasks().is_err() {
            return false;
        }
    }
    true
}

#[cfg(target_arch = "riscv64")]
extern "C" fn gate_consumer(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let mut method = i32::MIN;
    let transport = unsafe {
        abi::kcore_endpoint_call(
            state.endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            state.control.as_ptr().cast_mut().cast(),
            8,
            &mut method,
        )
    };
    state.transport.store(
        if transport == 0 { method } else { transport },
        Ordering::Release,
    );
    management::exit_task();
}

#[cfg(target_arch = "riscv64")]
fn gate_stop_race(state: *mut State) -> Option<()> {
    let provider = create(GATE_ONLY)?;
    let endpoint = lookup(provider)?;
    unsafe { core::ptr::addr_of_mut!((*state).endpoint).write(endpoint) };
    let task = start(gate_consumer, state, 1)?;
    let shared = unsafe { &*state };
    let end = deadline();
    while shared.control[0].load(Ordering::Acquire) == 0 {
        if unsafe { abi::kcore_now() } >= end {
            shared.control[1].store(1, Ordering::Release);
            return None;
        }
        core::hint::spin_loop();
    }
    // Provider owns no tasks. CPU1 is executing its Gate on a consumer task.
    let busy = unsafe { abi::kcore_component_stop(provider) } == Errno::EBUSY.code()
        && super::deployment::state(provider) == Some(3);
    shared.control[1].store(1, Ordering::Release);
    if !await_exit(task)
        || !busy
        || shared.transport.load(Ordering::Acquire) != 0
        || unsafe { abi::kcore_component_stop(provider) } != 0
    {
        return None;
    }
    let mut status = 123;
    if unsafe {
        abi::kcore_endpoint_call(
            endpoint,
            0,
            core::ptr::null(),
            0,
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
            0,
            &mut status,
        )
    } != Errno::ENOENT.code()
        || status != 123
        || unsafe { abi::kcore_component_stop(provider) } != Errno::EINVAL.code()
    {
        return None;
    }
    let restarted = create(GATE_ONLY)?;
    if restarted == provider
        || lookup(restarted)? == endpoint
        || unsafe { abi::kcore_component_stop(restarted) } != 0
    {
        return None;
    }
    Some(())
}

pub fn group(checks: &mut Checks, state: *mut State) {
    checks.group("component convergence");
    unsafe {
        state.write(State {
            providers: [0; 4],
            result: 0,
            #[cfg(target_arch = "riscv64")]
            endpoint: 0,
            #[cfg(target_arch = "riscv64")]
            control: [AtomicU32::new(0), AtomicU32::new(0)],
            #[cfg(target_arch = "riscv64")]
            transport: AtomicI32::new(i32::MIN),
        })
    };
    let before = unsafe { abi::kcore_task_count() };
    for (index, mode) in [PASSIVE, PASSIVE, ACTIVE, HYBRID].into_iter().enumerate() {
        let Some(provider) = create(mode) else {
            checks.check("checksum-create", false);
            return;
        };
        unsafe { (*state).providers[index] = provider };
        if index == 1 {
            checks.check(
                "passive-no-tasks",
                unsafe { abi::kcore_task_count() } == before
                    && unsafe { (*state).providers[0] != provider },
            );
        }
    }
    let done = start(consumer, state, 0).is_some_and(await_exit);
    let result = if done { unsafe { (*state).result } } else { 0 };
    for (name, mask) in [
        ("passive-independent-state", 1),
        ("direct-ctx-pinned", 2),
        ("active-runtime-reply", 4),
        ("hybrid-direct-worker", 8),
        ("worker-owner", 16),
        ("nested-lifecycle-switch-denied", 32),
        ("direct-gate-echo-baseline", 64),
    ] {
        checks.check(name, result & mask != 0);
    }
    for index in 2..4 {
        let exited = bind(unsafe { (*state).providers[index] })
            .is_some_and(|binding| await_exit((binding.api.worker)(binding.ctx)));
        checks.check(
            if index == 2 {
                "active-worker-exited"
            } else {
                "hybrid-worker-exited"
            },
            exited,
        );
    }
    #[cfg(target_arch = "riscv64")]
    checks.check("smp-gate-stop-admission", gate_stop_race(state).is_some());
}
