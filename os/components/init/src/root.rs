use kcomp_sdk::abi::{EndpointInfo, EndpointState};
use kcomp_sdk::block::BlockDevice;
use kcomp_sdk::endpoint::Contract;

/// Explicit profile policy: select the requested ordinal among live exact block endpoints.
/// Enumeration order is part of this boot profile, not a global uniqueness constraint.
pub fn observe(wanted: &mut u32, selected: Option<u64>, row: &EndpointInfo) -> Option<u64> {
    if selected.is_some()
        || row.state != EndpointState::Live as u32
        || row.contract != BlockDevice::ID
        || row.abi != BlockDevice::ABI
    {
        return selected;
    }
    if *wanted == 0 {
        Some(row.id)
    } else {
        *wanted -= 1;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: u64) -> EndpointInfo {
        EndpointInfo {
            id,
            contract: BlockDevice::ID,
            abi: BlockDevice::ABI,
            provider: 1,
            port: 0,
            state: EndpointState::Live as u32,
            name_len: 0,
        }
    }

    #[test]
    fn explicit_root_ordinal_accepts_multiple_providers() {
        let mut ordinal = 1;
        let selected = observe(&mut ordinal, None, &block(7));
        assert_eq!(selected, None);
        let selected = observe(&mut ordinal, selected, &block(8));
        assert_eq!(selected, Some(8));
        assert_eq!(observe(&mut ordinal, selected, &block(9)), Some(8));
    }

    #[test]
    fn dead_pending_and_incompatible_endpoints_cannot_be_roots() {
        for state in [EndpointState::Pending, EndpointState::Invalid] {
            let row = EndpointInfo {
                state: state as u32,
                ..block(7)
            };
            let mut wanted = 1;
            assert_eq!(observe(&mut wanted, None, &row), None);
            assert_eq!(wanted, 1);
        }
        let wrong_contract = EndpointInfo {
            contract: BlockDevice::ID ^ 1,
            ..block(7)
        };
        let wrong_abi = EndpointInfo {
            abi: BlockDevice::ABI ^ 1,
            ..block(7)
        };
        assert_eq!(observe(&mut 0, Some(8), &wrong_contract), Some(8));
        assert_eq!(observe(&mut 0, Some(8), &wrong_abi), Some(8));
    }
}
