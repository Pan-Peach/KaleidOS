//! One allocation-using artifact, two deployments and independent private heaps.
use super::*;
use kernel::component::{
    containment::KcompCreateArgs, endpoint::ExecutionDomain, exit, load, registry, ComponentState,
};

fn create(domain: ExecutionDomain, seed: u8) -> ComponentId {
    let args = KcompCreateArgs {
        config_abi: 0,
        config: (&seed as *const u8).cast(),
        config_len: 1,
    };
    match load::create_component(b"kcomp_heap", &args, domain) {
        Ok(id) => id,
        Err(error) => {
            panic!("isolated-heap: {:?} create failed: {:?}", domain, error);
        }
    }
}

fn report(id: ComponentId) -> (Option<AddressSpaceHandle>, usize, usize) {
    let (handle, state) = {
        let reg = registry::get_registry().lock();
        let record = reg.get(id).unwrap();
        (record.address_space, record.instance_state as usize)
    };
    let read = |offset| {
        let address = state + offset;
        let pa = match handle {
            Some(handle) => address_space::translate(handle, address).unwrap().unwrap(),
            None => address,
        };
        // SAFETY: live instance's C report prefix, translated through its real AS.
        unsafe { (pa as *const usize).read() }
    };
    (handle, read(0), read(core::mem::size_of::<usize>()))
}

fn verify(handle: Option<AddressSpaceHandle>, buffer: usize, len: usize, seed: u8) {
    if len != 8192 {
        fail("isolated-heap: report length");
    }
    for offset in 0..len {
        let address = buffer + offset;
        let pa = match handle {
            Some(handle) => address_space::translate(handle, address).unwrap().unwrap(),
            None => address,
        };
        // SAFETY: report describes the live allocation; check across page boundaries.
        if unsafe { (pa as *const u8).read() } != seed {
            fail("isolated-heap: data corrupted");
        }
    }
}

pub(crate) fn isolated_heap() -> ! {
    let core_satp = read_satp();
    let native = create(ExecutionDomain::KernelNative, 0x11);
    let first = create(ExecutionDomain::IsolatedNative, 0x22);
    let second = create(ExecutionDomain::IsolatedNative, 0x33);
    let (native_as, native_buf, native_len) = report(native);
    let (first_as, first_buf, first_len) = report(first);
    let (second_as, second_buf, second_len) = report(second);
    if native_as.is_some() || first_as.is_none() || second_as.is_none() || first_as == second_as {
        fail("isolated-heap: deployment truth");
    }
    if first_buf < 0x2300_0000 || first_buf >= 0x2f00_0000 {
        fail("isolated-heap: heap is not local VA");
    }
    let first_as = first_as.unwrap();
    let second_as = second_as.unwrap();
    let first_pa = address_space::translate(first_as, first_buf)
        .unwrap()
        .unwrap();
    let second_pa = address_space::translate(second_as, second_buf)
        .unwrap()
        .unwrap();
    if first_pa == second_pa {
        fail("isolated-heap: backing shared between instances");
    }
    if address_space::translate(second_as, first_pa)
        .unwrap()
        .is_some()
        || address_space::translate(first_as, second_pa)
            .unwrap()
            .is_some()
    {
        fail("isolated-heap: private backing exposed through identity alias");
    }
    verify(native_as, native_buf, native_len, 0x11);
    verify(Some(first_as), first_buf, first_len, 0x22);
    verify(Some(second_as), second_buf, second_len, 0x33);
    for id in [native, first] {
        if exit::stop_component(id).is_err() {
            fail("isolated-heap: destroy failed");
        }
        if registry::get_registry().lock().get(id).unwrap().state != ComponentState::Stopped {
            fail("isolated-heap: stopped state");
        }
    }
    // Retiring one heap cannot alter a live sibling's objects or allocator state.
    verify(Some(second_as), second_buf, second_len, 0x33);
    let restarted = create(ExecutionDomain::IsolatedNative, 0x44);
    let (new_as, new_buf, new_len) = report(restarted);
    verify(new_as, new_buf, new_len, 0x44);
    verify(Some(second_as), second_buf, second_len, 0x33);
    for id in [second, restarted] {
        if exit::stop_component(id).is_err() {
            fail("isolated-heap: final destroy failed");
        }
    }
    if read_satp() != core_satp {
        fail("isolated-heap: Core satp not restored");
    }
    kernel::log!(
        "selftest",
        "isolated-heap: same artifact K/I, private heaps, growth/release/restart OK"
    );
    pass("isolated-heap")
}
