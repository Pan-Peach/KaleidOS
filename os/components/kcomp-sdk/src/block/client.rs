//! Typed IPC Block binding. A transfer is split into bounded 512-byte requests.
use crate::{
    Errno, abi,
    block::BlockDevice,
    endpoint::{Contract, Endpoint, InvokeError},
    generated::block_wire as wire,
};
pub struct BlockBinding {
    endpoint: u64,
}
impl Endpoint<BlockDevice> {
    pub fn bind(&self) -> Result<BlockBinding, InvokeError> {
        BlockBinding::connect(self.id())
    }
}
impl BlockBinding {
    pub fn connect(endpoint: u64) -> Result<Self, InvokeError> {
        let (mut mechanism, mut api, mut ctx) = (0, 0, 0);
        let rc = unsafe {
            abi::kcore_endpoint_bind(
                endpoint,
                BlockDevice::ID,
                BlockDevice::ABI,
                &mut mechanism,
                &mut api,
                &mut ctx,
            )
        };
        if rc != 0 {
            return Err(InvokeError::Transport(Errno::from_code(rc)));
        }
        if mechanism != abi::KCORE_ENDPOINT_MECHANISM_IPC || api != 0 || ctx != 0 {
            return Err(InvokeError::Transport(Errno::ENOTSUP));
        }
        Ok(Self { endpoint })
    }
    pub fn capacity_sectors(&self) -> Result<u64, InvokeError> {
        wire::capacity_sectors(self.endpoint)
    }
    pub fn read(&self, lba: u64, output: &mut [u8]) -> Result<(), InvokeError> {
        check_range(lba, output.len())?;
        for (index, sector) in output.as_chunks_mut::<512>().0.iter_mut().enumerate() {
            wire::read(self.endpoint, lba + index as u64, sector)?;
        }
        Ok(())
    }
    pub fn write(&self, lba: u64, input: &[u8]) -> Result<(), InvokeError> {
        check_range(lba, input.len())?;
        for (index, sector) in input.as_chunks::<512>().0.iter().enumerate() {
            wire::write(self.endpoint, lba + index as u64, sector)?;
        }
        Ok(())
    }
}
fn check_range(lba: u64, len: usize) -> Result<(), InvokeError> {
    if len == 0 || !len.is_multiple_of(512) {
        return Err(InvokeError::Method(Errno::EINVAL));
    }
    lba.checked_add((len / 512 - 1) as u64)
        .map(|_| ())
        .ok_or(InvokeError::Method(Errno::EOVERFLOW))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_length_and_lba_overflow_fail_before_any_request() {
        for len in [0, 1, 511, 513] {
            assert_eq!(check_range(0, len), Err(InvokeError::Method(Errno::EINVAL)));
        }
        assert!(check_range(u64::MAX, 512).is_ok());
        assert_eq!(
            check_range(u64::MAX, 1024),
            Err(InvokeError::Method(Errno::EOVERFLOW))
        );
    }
}
