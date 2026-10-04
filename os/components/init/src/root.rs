use kcomp_sdk::abi::{EndpointInfo, EndpointState};
use kcomp_sdk::block::BlockDevice;
use kcomp_sdk::endpoint::Contract;
use kcomp_sdk::{Errno, Result};

/// This profile accepts one live block provider; multiple candidates need policy.
pub fn observe(selected: Option<u64>, row: &EndpointInfo) -> Result<Option<u64>> {
    if row.state != EndpointState::Live as u32
        || row.contract != BlockDevice::ID
        || row.abi != BlockDevice::ABI
    {
        return Ok(selected);
    }
    if selected.is_some() {
        return Err(Errno::EBUSY);
    }
    Ok(Some(row.id))
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
    fn root_requires_an_unambiguous_provider() {
        let selected = observe(None, &block(7)).unwrap();
        assert_eq!(selected, Some(7));
        assert_eq!(observe(selected, &block(8)), Err(Errno::EBUSY));
    }

    #[test]
    fn dead_pending_and_incompatible_endpoints_cannot_be_roots() {
        for state in [EndpointState::Pending, EndpointState::Invalid] {
            let row = EndpointInfo {
                state: state as u32,
                ..block(7)
            };
            assert_eq!(observe(None, &row), Ok(None));
        }
        let wrong_contract = EndpointInfo {
            contract: BlockDevice::ID ^ 1,
            ..block(7)
        };
        let wrong_abi = EndpointInfo {
            abi: BlockDevice::ABI ^ 1,
            ..block(7)
        };
        assert_eq!(observe(Some(8), &wrong_contract), Ok(Some(8)));
        assert_eq!(observe(Some(8), &wrong_abi), Ok(Some(8)));
    }
}
