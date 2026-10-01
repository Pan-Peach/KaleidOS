//! Deliberately failing task owner. CoreTest orchestrates all SMP checks;
//! this separate image lets it survive the failure and inspect Core truth.
#![no_std]

use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::{abi, errno::Errno, mem};

const CONFIG_ABI: u64 = 0x534d_5050_414e_4943; // "SMPPANIC"

#[repr(C)]
struct Config {
    words: *mut u32,
    cpu: u32,
}

struct State {
    config: Config,
    region: abi::MemoryView,
}

extern "C" fn failing_task(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let cfg = &state.config;
    assert_eq!(unsafe { abi::kcore_cpu_current() }, cfg.cpu);
    // Config is a C-layout window; both images access its u32 words atomically.
    let ready = unsafe { AtomicU32::from_ptr(cfg.words) };
    ready.fetch_or(1 << cfg.cpu, Ordering::AcqRel);
    let end = unsafe { abi::kcore_now().saturating_add(abi::kcore_timebase_hz() * 3) };
    while ready.load(Ordering::Acquire) != 3 {
        assert!(
            unsafe { abi::kcore_now() } < end,
            "panic rendezvous timed out"
        );
        core::hint::spin_loop();
    }
    panic!("kcomp_smp: deliberate task panic");
}

kcomp_sdk::kcomp_instance_create!(|args, out_state| {
    let args = unsafe { &*args };
    if args.config_abi != CONFIG_ABI
        || args.config_len != core::mem::size_of::<Config>()
        || args.config.is_null()
    {
        return Errno::EINVAL.code();
    }
    let config = unsafe { args.config.cast::<Config>().read_unaligned() };
    if config.words.is_null() || config.cpu > 1 {
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
        core::ptr::addr_of_mut!((*state).config).write(config);
        core::ptr::addr_of_mut!((*state).region).write(region);
        *out_state = state.cast();
    }
    let mut id = u32::MAX;
    let result = unsafe { abi::kcore_task_create(failing_task, state.cast(), &mut id) };
    if result != 0 {
        return result;
    }
    unsafe { AtomicU32::from_ptr((*state).config.words.add(6 + (*state).config.cpu as usize)) }
        .store(id, Ordering::Release);
    unsafe { abi::kcore_task_start_on(id, (*state).config.cpu) }
});

kcomp_sdk::kcomp_instance_destroy!(|state| {
    match mem::mem_release(unsafe { (*state.cast::<State>()).region }) {
        Ok(()) => 0,
        Err(error) => error.code(),
    }
});
