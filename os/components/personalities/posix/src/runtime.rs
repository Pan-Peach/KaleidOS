//! Instantiate one immutable-image process family, not one component per PID.
use crate::{execution::Family, image};
use core::sync::atomic::Ordering;
use kcomp_sdk::{Errno, posix::KCOMP_POSIX_CREATE_CONFIG_ABI};

kcomp_sdk::kcomp_instance_create!(|args, out_state| {
    let Some(args) = (unsafe { args.as_ref() }) else {
        return Errno::EINVAL.code();
    };
    if out_state.is_null()
        || args.config_abi != KCOMP_POSIX_CREATE_CONFIG_ABI
        || args.config.is_null()
        || args.config_len > 16 * 1024 * 1024
    {
        return Errno::EINVAL.code();
    }
    let bytes = unsafe { core::slice::from_raw_parts(args.config.cast::<u8>(), args.config_len) };
    match image::decode(bytes).and_then(crate::execution::start) {
        Ok(family) => {
            unsafe { *out_state = family.cast() };
            0
        }
        Err(error) => error.code(),
    }
});
kcomp_sdk::kcomp_instance_destroy!(|state| {
    let Some(family) = (unsafe { state.cast::<Family>().as_ref() }) else {
        return 0;
    };
    if family.live.load(Ordering::Acquire) != 0 {
        return Errno::EBUSY.code();
    }
    family.alive.store(false, Ordering::Release);
    // Direct observer ctx remains resident after logical teardown.
    0
});
const PROCESS_PORT: u32 = 0;
kcomp_sdk::kcomp_services!(state: Family; PROCESS_PORT => crate::execution::dispatch);
