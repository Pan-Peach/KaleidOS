//! Public deployment and lifecycle facts; hardware enforcement stays in ArchTest.
use super::{report::Checks, trace};
use kcomp_sdk::{
    Errno,
    abi::ExecutionDomain,
    endpoint::{Endpoint, InvokeError},
    management,
};

#[path = "../../../domain_contract.rs"]
mod domain;
use domain::LegacyBind;

fn bind(id: u32) -> Option<domain::Binding> {
    Endpoint::<domain::DomainService>::lookup(id, domain::DOMAIN_NAME)
        .ok()?
        .legacy_bind()
        .ok()
}

fn healthy(id: u32) -> bool {
    let Some(binding) = bind(id) else {
        return false;
    };
    let mut output = [0; 512];
    binding.capacity_sectors() == Ok(64)
        && binding.read(3, &mut output).is_ok()
        && output == [0x41 ^ 3; 512]
        && binding.write(3, &output).is_ok()
}

fn exercise(caller: u32, providers: [u32; 2], mode: u32) -> bool {
    let Some(binding) = bind(caller) else {
        return false;
    };
    let mut input = [0; 512];
    for (index, word) in [providers[0], providers[1], mode].into_iter().enumerate() {
        input[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    binding.write(u64::MAX - 2, &input).is_ok()
}

fn relay(provider: u32, target: u32) -> bool {
    let Some(binding) = bind(provider) else {
        return false;
    };
    let mut input = [0; 512];
    input[..4].copy_from_slice(&target.to_le_bytes());
    binding.write(u64::MAX - 1, &input).is_ok()
}

use super::component_state as state;

fn services(checks: &mut Checks) -> Option<()> {
    use ExecutionDomain::{IsolatedNative as I, KernelNative as K};
    let native = management::load(b"kcomp_domain_service", K).ok()?;
    let mut config = [0; 16];
    // Fixture config: plain provider, mode 0, seed 0x41, no relay.
    config[8..12].copy_from_slice(&0x41u32.to_ne_bytes());
    let private = management::create(b"kcomp_domain_service", I, 0, &config).ok()?;
    let caller = management::load(b"kcomp_domain_service", I).ok()?;
    checks.check("service-native-caller", healthy(native) && healthy(private));
    checks.check(
        "service-isolated-caller",
        exercise(caller, [native, private], 0),
    );

    let leaf = management::load(b"kcomp_domain_service", I).ok()?;
    checks.check(
        "service-nested-domains",
        relay(native, leaf) && exercise(caller, [native, private], 0),
    );
    let other = management::load(b"kcomp_domain_service", I).ok()?;
    checks.check(
        "service-reentry-rejected",
        relay(leaf, other) && relay(other, leaf) && exercise(caller, [leaf, other], 2),
    );
    checks.check(
        "service-reentry-recovery",
        relay(leaf, 0) && relay(other, 0) && healthy(leaf) && healthy(other),
    );
    checks.check(
        "service-provider-panic",
        relay(native, 0)
            && exercise(caller, [native, private], 1)
            && state(native) == Some(6)
            && state(private) == Some(6)
            && state(caller) == Some(3),
    );
    let old = bind(leaf)?;
    // The private leaf can also fail through a gate from this native caller.
    let mut output = [0xce; 512];
    let contained = old.read(u64::MAX, &mut output) == Err(InvokeError::Transport(Errno::EIO));
    let from = trace::cursor();
    let fresh = management::load(b"kcomp_domain_service", I).ok()?;
    checks.check(
        "service-fresh-instance",
        contained
            && output == [0xce; 512]
            && fresh != leaf
            && trace::component_lifecycle(from, fresh as i32)
            && healthy(fresh),
    );
    checks.check(
        "service-stale-binding",
        old.read(0, &mut output) == Err(InvokeError::Transport(Errno::ENOENT)),
    );
    Some(())
}

pub fn group(checks: &mut Checks) {
    use ExecutionDomain::{IsolatedNative as I, KernelNative as K, SandboxedNative as U};
    checks.group("deployment");
    let mut ids = [0; 3];
    let mut heaps_ok = true;
    for (index, domain) in [K, I, I].into_iter().enumerate() {
        let from = trace::cursor();
        match management::load(b"kcomp_heap", domain) {
            Ok(id) => {
                ids[index] = id;
                heaps_ok &= trace::component_lifecycle(from, id as i32);
            }
            Err(_) => heaps_ok = false,
        }
    }
    checks.check(
        "heap-deployments",
        heaps_ok && ids[0] != ids[1] && ids[1] != ids[2],
    );
    checks.check(
        "sandboxed-load-validation",
        if cfg!(target_arch = "riscv64") {
            management::load(b"kcomp_heap", U)
                .is_ok_and(|id| unsafe { kcomp_sdk::abi::kcore_component_stop(id) } == 0)
        } else {
            management::load(b"kcomp_heap", U) == Err(Errno::ENOTSUP)
        },
    );
    let completed = services(checks).is_some();
    checks.check("service-scenarios-completed", completed);
}
