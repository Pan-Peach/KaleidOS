//! One artifact / instance model: Passive, Active, Hybrid, and a Gate-only probe.
#![no_std]

#[path = "../contract.rs"]
#[allow(dead_code)]
mod contract;

use contract::*;
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, Ordering},
};
use kcomp_sdk::{Errno, abi, management, mem};

struct State {
    mailbox: UnsafeCell<Mailbox>,
    worker: u32,
    mode: u32,
    region: abi::MemoryView,
}

fn checksum(value: u32) -> u32 {
    (value & 255) + ((value >> 8) & 255) + ((value >> 16) & 255) + (value >> 24)
}

extern "C" fn direct(ctx: *mut (), value: u32) -> u32 {
    let state = unsafe { &*ctx.cast::<State>() };
    if state.mode == ACTIVE {
        return 0;
    }
    let mailbox = state.mailbox.get();
    unsafe { AtomicU32::from_ptr(core::ptr::addr_of_mut!((*mailbox).direct_calls)) }
        .fetch_add(1, Ordering::Relaxed);
    checksum(value)
}

extern "C" fn mailbox(ctx: *mut ()) -> *mut Mailbox {
    unsafe { (*ctx.cast::<State>()).mailbox.get() }
}

extern "C" fn worker_id(ctx: *mut ()) -> u32 {
    unsafe { (*ctx.cast::<State>()).worker }
}

static API: Api = Api {
    checksum: direct,
    mailbox,
    worker: worker_id,
    echo,
};

// Test-only copy workload shared by Direct and Gate measurements. The caller
// provides disjoint readable/writable buffers, borrowed until return.
unsafe extern "C" fn echo(_ctx: *mut (), input: *const u8, output: *mut u8, len: usize) -> i32 {
    if len > ECHO_MAX || (len != 0 && (input.is_null() || output.is_null())) {
        return Errno::EINVAL.code();
    }
    if len != 0 {
        unsafe { core::ptr::copy_nonoverlapping(input, output, len) };
    }
    0
}

extern "C" fn worker(ctx: *mut ()) {
    let mailbox = mailbox(ctx);
    let phase = unsafe { AtomicU32::from_ptr(core::ptr::addr_of_mut!((*mailbox).phase)) };
    let stop = unsafe { AtomicU32::from_ptr(core::ptr::addr_of_mut!((*mailbox).stop)) };
    let end = unsafe { abi::kcore_now().saturating_add(abi::kcore_timebase_hz() * 10) };
    loop {
        if phase.load(Ordering::Acquire) == 2 {
            let value = unsafe { AtomicU32::from_ptr(core::ptr::addr_of_mut!((*mailbox).value)) }
                .load(Ordering::Relaxed);
            unsafe { AtomicU32::from_ptr(core::ptr::addr_of_mut!((*mailbox).result)) }
                .store(checksum(value), Ordering::Relaxed);
            unsafe { AtomicU32::from_ptr(core::ptr::addr_of_mut!((*mailbox).worker_calls)) }
                .fetch_add(1, Ordering::Relaxed);
            phase.store(3, Ordering::Release);
        }
        if stop.load(Ordering::Acquire) != 0 || unsafe { abi::kcore_now() } >= end {
            management::exit_task();
        }
        // No cross-owner wake primitive is assumed: this bounded experiment
        // uses the existing cooperative scheduler, not a new waitqueue.
        assert!(management::yield_task().is_ok());
    }
}

fn lifecycle_switches_denied() -> bool {
    unsafe {
        abi::kcore_task_yield() == Errno::EINVAL.code()
            && abi::kcore_task_park() == Errno::EINVAL.code()
            && abi::kcore_task_exit() == Errno::EINVAL.code()
    }
}

kcomp_sdk::kcomp_instance_create!(|args, out_state| {
    let args = unsafe { &*args };
    if args.config_abi != CONFIG_ABI || args.config_len != 8 || args.config.is_null() {
        return Errno::EINVAL.code();
    }
    let words = args.config.cast::<u32>();
    let mode = unsafe { words.read_unaligned() };
    let cpu = unsafe { words.add(1).read_unaligned() };
    if mode > LIFECYCLE_PROBE {
        return Errno::EINVAL.code();
    }
    let region = match mem::mem_alloc(
        core::mem::size_of::<State>() as u64,
        core::mem::align_of::<State>() as u64,
    ) {
        Ok(region) => region,
        Err(error) => return error.code(),
    };
    let state = region.base as *mut State;
    unsafe {
        state.write(State {
            mailbox: UnsafeCell::new(Mailbox {
                phase: 0,
                value: 0,
                result: 0,
                direct_calls: 0,
                worker_calls: 0,
                stop: 0,
            }),
            worker: u32::MAX,
            mode,
            region,
        });
        *out_state = state.cast();
    }
    if mode == LIFECYCLE_PROBE && !lifecycle_switches_denied() {
        return Errno::EIO.code();
    }
    if mode == ACTIVE || mode == HYBRID {
        let mut task = 0;
        let code = unsafe { abi::kcore_task_create(worker, state.cast(), &mut task) };
        if code != 0 {
            let _ = mem::mem_release(region);
            return code;
        }
        unsafe { core::ptr::addr_of_mut!((*state).worker).write(task) };
        let code = unsafe { abi::kcore_task_start_on(task, cpu) };
        if code != 0 {
            // The Created task still borrows state: retain it on init failure.
            return code;
        }
    }
    let code = unsafe {
        abi::kcore_endpoint_publish(
            NAME.as_ptr(),
            NAME.len(),
            CONTRACT,
            1,
            ABI,
            0,
            if mode == GATE_ONLY || mode == LIFECYCLE_PROBE {
                core::ptr::null()
            } else {
                core::ptr::from_ref(&API).cast()
            },
            state.cast(),
        )
    };
    if code != 0 {
        unsafe { AtomicU32::from_ptr(core::ptr::addr_of_mut!((*(*state).mailbox.get()).stop)) }
            .store(1, Ordering::Release);
    }
    code
});

kcomp_sdk::kcomp_instance_destroy!(|state| {
    // Exercise an Exit boundary nested over the caller Task, too.
    if unsafe { (*state.cast::<State>()).mode } == LIFECYCLE_PROBE && !lifecycle_switches_denied() {
        return Errno::EIO.code();
    }
    // Direct instances never reach destroy; Gate-only instances have no worker.
    match mem::mem_release(unsafe { (*state.cast::<State>()).region }) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
});

/// Gate-only probe holds a real provider call until the consumer releases it.
/// Output is a C-layout pair of atomic u32 control words, borrowed for this call.
///
/// # Safety
/// Core must supply a readable frame and two writable, aligned control words;
/// their backing must remain valid until this synchronous call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kcomp_service_dispatch(
    _state: *mut (),
    port: u32,
    method: u32,
    frame: *const abi::KcompCallFrame,
) -> i32 {
    if port != 0 || frame.is_null() {
        return Errno::EINVAL.code();
    }
    let frame = unsafe { &*frame };
    if method == ECHO {
        if frame.args_len != 0 || frame.input_len != frame.output_len {
            return Errno::EINVAL.code();
        }
        return unsafe { echo(_state, frame.input, frame.output, frame.input_len) };
    }
    if method != 0 {
        return Errno::EINVAL.code();
    }
    if frame.output.is_null() || frame.output_len != 8 || !(frame.output as usize).is_multiple_of(4)
    {
        return Errno::EINVAL.code();
    }
    let entered = unsafe { AtomicU32::from_ptr(frame.output.cast()) };
    let release = unsafe { AtomicU32::from_ptr(frame.output.cast::<u32>().add(1)) };
    entered.store(1, Ordering::Release);
    let end = unsafe { abi::kcore_now().saturating_add(abi::kcore_timebase_hz() * 5) };
    while release.load(Ordering::Acquire) == 0 {
        if unsafe { abi::kcore_now() } >= end {
            return Errno::ETIMEDOUT.code();
        }
        core::hint::spin_loop();
    }
    0
}
