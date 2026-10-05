//! Real SDK provider and consumer, one artifact across the K/I deployment matrix.
use super::*;
// Contract semantics stay outside Core. This hardware test uses the SDK's
// generated wire constants without linking a component runtime into the kernel.
#[allow(dead_code)]
mod block {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../components/kcomp-sdk/src/generated/block.rs"
    ));
}
use kernel::component::{
    ComponentState, call,
    containment::KcompCreateArgs,
    endpoint::{self, ExecutionDomain},
    exit, load, registry,
};

fn create(domain: ExecutionDomain, provider: u32, mode: u32, seed: u32, relay: u32) -> ComponentId {
    let config = [provider, mode, seed, relay];
    let args = KcompCreateArgs {
        config_abi: 0,
        config: config.as_ptr().cast(),
        config_len: core::mem::size_of_val(&config),
    };
    match load::create_component(b"kcomp_domain_service", &args, domain) {
        Ok(id) => id,
        Err(error) => panic!(
            "isolated-domain-service: {:?}, mode {}: {:?}",
            domain, mode, error
        ),
    }
}

fn endpoint(id: ComponentId) -> endpoint::EndpointId {
    let reg = registry::get_registry().lock();
    endpoint::get_endpoints()
        .lock()
        .discover(
            &reg,
            id,
            b"block.device",
            endpoint::ContractId::from_raw(block::KCOMP_BLOCK_DEVICE_CONTRACT),
        )
        .unwrap()
}

fn verify(id: ComponentId, expected: u8, core_satp: usize) {
    let (space, state) = {
        let reg = registry::get_registry().lock();
        let record = reg.get(id).unwrap();
        assert_eq!(record.state, ComponentState::Ready);
        assert_eq!(reg.active_calls(id), 0);
        (record.address_space, record.instance_state as usize)
    };
    let physical = |va| match space {
        Some(space) => address_space::translate(space, va).unwrap().unwrap(),
        None => va,
    };
    let slot = |index| unsafe {
        (physical(state + index * core::mem::size_of::<usize>()) as *const usize).read()
    };
    let (buffer, len, before, after) = (slot(0), slot(1), slot(2), slot(3));
    let root = match space {
        Some(space) => address_space::prepare_activation(space)
            .unwrap()
            .token()
            .satp(),
        None => core_satp,
    };
    assert_eq!(before, root);
    assert_eq!(after, root);
    assert_eq!(len, 8192);
    for offset in 0..len {
        assert_eq!(
            unsafe { (physical(buffer + offset) as *const u8).read() },
            expected
        );
    }
    assert_ne!(endpoint(id).raw(), 0, "provider published through the SDK");
}

pub(crate) fn isolated_domain_service() -> ! {
    use ExecutionDomain::{IsolatedNative as I, KernelNative as K};
    let core_satp = read_satp();
    for (caller_domain, provider_domain) in [(K, K), (K, I), (I, K), (I, I)] {
        let provider = create(provider_domain, 0, 0, 0x51, 0);
        let caller = create(caller_domain, provider.raw(), 0, 0x51, 0);
        verify(caller, 0x51 ^ 3, core_satp);
        verify(provider, 0xce, core_satp);
        assert_eq!(read_satp(), core_satp);
        for id in [caller, provider] {
            exit::stop_component(id).unwrap();
        }
    }

    // Nested I -> K -> I restores both private caller stacks and the Core root.
    let leaf = create(I, 0, 0, 0x72, 0);
    let relay = create(K, 0, 0, 0x72, leaf.raw());
    let caller = create(I, relay.raw(), 0, 0x72, 0);
    verify(caller, 0x72 ^ 3, core_satp);
    for id in [caller, relay, leaf] {
        exit::stop_component(id).unwrap();
    }

    // I callers survive either native or isolated provider panic; their private
    // output is unchanged, and the failed endpoint is permanently invalid.
    for provider_domain in [K, I] {
        let provider = create(provider_domain, 0, 0, 0x33, 0);
        let old_endpoint = endpoint(provider);
        let caller = create(I, provider.raw(), 1, 0x33, 0);
        verify(caller, 0xce, core_satp);
        let reg = registry::get_registry().lock();
        assert_eq!(reg.get(provider).unwrap().state, ComponentState::Failed);
        assert_eq!(reg.active_calls(provider), 0);
        assert!(
            endpoint::get_endpoints()
                .lock()
                .resolve(&reg, old_endpoint)
                .is_err()
        );
        drop(reg);
        exit::stop_component(caller).unwrap();
    }

    // An invalid caller-private input is rejected before provider dispatch.
    let provider = create(I, 0, 0, 0x41, 0);
    let caller = create(I, provider.raw(), 3, 0x41, 0);
    verify(caller, 0xce, core_satp);
    exit::stop_component(caller).unwrap();

    // I A -> I B -> I A cannot reenter A's in-use private stack.
    let other = create(I, 0, 0, 0x41, provider.raw());
    let native = load::load_and_start(b"kcomp_smoke", K).unwrap();
    let mut config = [0u8; 512];
    config[..4].copy_from_slice(&other.raw().to_le_bytes());
    let args = (u64::MAX - 1).to_le_bytes();
    let mut status = -1;
    assert_eq!(
        with_kernel_caller(native, 0x7b, || call::endpoint_call(
            endpoint(provider),
            block::KCOMP_BLOCK_METHOD_WRITE,
            args.as_ptr(),
            args.len(),
            config.as_ptr(),
            config.len(),
            core::ptr::null_mut(),
            0,
            &mut status
        )),
        Ok(())
    );
    assert_eq!(status, 0);
    let caller = create(I, provider.raw(), 2, 0x41, 0);
    verify(caller, 0xce, core_satp);
    for id in [provider, other] {
        verify(id, 0xce, core_satp);
    }
    for id in [caller, other, provider, native] {
        exit::stop_component(id).unwrap();
    }
    assert_eq!(read_satp(), core_satp);
    kernel::log!(
        "selftest",
        "isolated-domain-service: K/K K/I I/K I/I, nested calls, panic/stale/reentry OK"
    );
    pass("isolated-domain-service")
}
