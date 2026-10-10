//! Synchronous fixture adapter retained only to test real private AS/fault entry.
use crate::domain::{
    DOMAIN_NAME, DomainApi, DomainService, METHOD_CAPACITY, METHOD_READ, METHOD_WRITE,
};
use kcomp_sdk::{Errno, Result, abi, endpoint::Contract, frame::Call};
pub trait Provider {
    fn capacity_sectors(&self) -> u64;
    fn read(&self, lba: u64, output: &mut [u8]) -> Result<()>;
    fn write(&self, lba: u64, input: &[u8]) -> Result<()>;
}
pub struct Service<P> {
    provider: P,
    api: DomainApi,
}
impl<P: Provider> Service<P> {
    pub const fn new(provider: P) -> Self {
        Self {
            provider,
            api: DomainApi {
                capacity_sectors: capacity::<P>,
                read: read::<P>,
                write: write::<P>,
            },
        }
    }
    pub fn publish_endpoint(&'static self, name: &[u8], port: u32) -> Result<()> {
        assert_eq!(name, DOMAIN_NAME);
        let rc = unsafe {
            abi::kcore_endpoint_publish(
                name.as_ptr(),
                name.len(),
                DomainService::ID,
                DomainService::KIND.as_u32(),
                DomainService::ABI,
                port,
                (&self.api as *const DomainApi).cast(),
                (&self.provider as *const P).cast_mut().cast(),
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(Errno::from_code(rc))
        }
    }
}
unsafe extern "C" fn capacity<P: Provider>(ctx: *mut ()) -> u64 {
    unsafe { (&*ctx.cast::<P>()).capacity_sectors() }
}
unsafe extern "C" fn read<P: Provider>(ctx: *mut (), lba: u64, output: *mut u8, len: usize) -> i32 {
    if ctx.is_null() || output.is_null() || !valid_len(len) {
        return Errno::EINVAL.code();
    }
    unsafe { (&*ctx.cast::<P>()).read(lba, core::slice::from_raw_parts_mut(output, len)) }
        .map_or_else(Errno::code, |()| 0)
}
unsafe extern "C" fn write<P: Provider>(
    ctx: *mut (),
    lba: u64,
    input: *const u8,
    len: usize,
) -> i32 {
    if ctx.is_null() || input.is_null() || !valid_len(len) {
        return Errno::EINVAL.code();
    }
    unsafe { (&*ctx.cast::<P>()).write(lba, core::slice::from_raw_parts(input, len)) }
        .map_or_else(Errno::code, |()| 0)
}
fn valid_len(len: usize) -> bool {
    len != 0 && len.is_multiple_of(512)
}
pub fn dispatch<P: Provider>(provider: &P, method: u32, call: Call<'_>) -> i32 {
    let result = match method {
        METHOD_CAPACITY
            if call.args.is_empty() && call.input.is_empty() && call.output.len() == 8 =>
        {
            call.output
                .copy_from_slice(&provider.capacity_sectors().to_le_bytes());
            Ok(())
        }
        METHOD_READ
            if call.args.len() == 8 && call.input.is_empty() && valid_len(call.output.len()) =>
        {
            provider.read(
                u64::from_le_bytes(call.args.try_into().unwrap()),
                call.output,
            )
        }
        METHOD_WRITE
            if call.args.len() == 8 && call.output.is_empty() && valid_len(call.input.len()) =>
        {
            provider.write(
                u64::from_le_bytes(call.args.try_into().unwrap()),
                call.input,
            )
        }
        METHOD_CAPACITY | METHOD_READ | METHOD_WRITE => Err(Errno::EINVAL),
        _ => Err(Errno::ENOSYS),
    };
    result.map_or_else(Errno::code, |()| 0)
}
