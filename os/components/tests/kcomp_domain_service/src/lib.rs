//! The same provider/consumer artifact exercises SDK bindings in K and I.
#![no_std]
extern crate alloc;

use alloc::{boxed::Box, vec};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use kcomp_sdk::{
    Errno,
    block::{BLOCK_DEVICE_NAME, BlockDevice, BlockDeviceProvider, BlockDeviceService},
    endpoint::{Endpoint, InvokeError},
    frame::Call,
};

const PORT: u32 = 1;
static SEED: AtomicU32 = AtomicU32::new(0);
static RELAY: AtomicU32 = AtomicU32::new(0);
static ROOT: AtomicUsize = AtomicUsize::new(0);

struct Device;
impl BlockDeviceProvider for Device {
    fn capacity_sectors(&self) -> u64 {
        64
    }
    fn read(&self, lba: u64, output: &mut [u8]) -> Result<(), Errno> {
        assert_eq!(satp(), ROOT.load(Ordering::Relaxed), "provider root");
        if lba == u64::MAX {
            panic!("domain service injected failure");
        }
        let relay = RELAY.load(Ordering::Relaxed);
        if relay != 0 {
            let binding = Endpoint::<BlockDevice>::lookup(relay, BLOCK_DEVICE_NAME)?
                .bind()
                .map_err(errno)?;
            return binding.read(lba, output).map_err(errno);
        }
        output.fill(SEED.load(Ordering::Relaxed) as u8 ^ lba as u8);
        Ok(())
    }
    fn write(&self, lba: u64, input: &[u8]) -> Result<(), Errno> {
        if lba == u64::MAX - 1 {
            RELAY.store(
                u32::from_le_bytes(input[..4].try_into().unwrap()),
                Ordering::Relaxed,
            );
            return Ok(());
        }
        if input
            .iter()
            .all(|byte| *byte == SEED.load(Ordering::Relaxed) as u8 ^ lba as u8)
        {
            Ok(())
        } else {
            Err(Errno::EINVAL)
        }
    }
}
static SERVICE: BlockDeviceService<Device> = BlockDeviceService::new(Device);

fn errno(error: InvokeError) -> Errno {
    match error {
        InvokeError::Method(errno) | InvokeError::Transport(errno) => errno,
        InvokeError::InvalidReply => Errno::EIO,
    }
}
fn satp() -> usize {
    let value;
    unsafe {
        core::arch::asm!("csrr {}, satp", out(reg) value, options(nostack));
    }
    value
}

#[repr(C)]
struct State {
    buffer: usize,
    len: usize,
    before: usize,
    after: usize,
    data: alloc::vec::Vec<u8>,
}

fn dispatch(_state: &State, method: u32, call: Call<'_>) -> i32 {
    kcomp_sdk::block::dispatch::dispatch(&Device, method, call)
}
kcomp_sdk::kcomp_services! { state: State; PORT => dispatch, }

kcomp_sdk::kcomp_instance_create!(|args, out_state| {
    let args = unsafe { &*args };
    if args.config_len != 16 || args.config.is_null() {
        return Errno::EINVAL.code();
    }
    let config = unsafe { core::slice::from_raw_parts(args.config.cast::<u32>(), 4) };
    let (provider, mode, seed, relay) = (config[0], config[1], config[2], config[3]);
    SEED.store(seed, Ordering::Relaxed);
    RELAY.store(relay, Ordering::Relaxed);
    let before = satp();
    ROOT.store(before, Ordering::Relaxed);
    let mut data = vec![0xceu8; 8192];
    if provider != 0 {
        let endpoint = match Endpoint::<BlockDevice>::lookup(provider, BLOCK_DEVICE_NAME) {
            Ok(endpoint) => endpoint,
            Err(error) => return error.code(),
        };
        let binding = match endpoint.bind() {
            Ok(binding) => binding,
            Err(error) => return errno(error).code(),
        };
        let result = match mode {
            0 => {
                if binding.capacity_sectors() != Ok(64) {
                    return Errno::EIO.code();
                }
                binding.read(3, &mut data).and_then(|()| {
                    if data.iter().any(|byte| *byte != seed as u8 ^ 3) {
                        return Err(InvokeError::InvalidReply);
                    }
                    binding.write(3, &data)
                })
            }
            1 => {
                if binding.read(u64::MAX, &mut data) != Err(InvokeError::Transport(Errno::EIO))
                    || data.iter().any(|byte| *byte != 0xce)
                    || binding.read(0, &mut data) != Err(InvokeError::Transport(Errno::ENOENT))
                {
                    return Errno::EIO.code();
                }
                Ok(())
            }
            2 => {
                // A -> B -> A must fail before A's private stack is reused.
                if binding.read(3, &mut data) != Err(InvokeError::Method(Errno::EBUSY)) {
                    return Errno::EIO.code();
                }
                Ok(())
            }
            3 => {
                let mut status = 123;
                let transport = unsafe {
                    kcomp_sdk::abi::kcore_endpoint_call(
                        endpoint.id(),
                        0,
                        0x3000_0000 as *const u8,
                        1,
                        core::ptr::null(),
                        0,
                        core::ptr::null_mut(),
                        0,
                        &mut status,
                    )
                };
                if transport != Errno::EFAULT.code() || status != 123 {
                    return Errno::EIO.code();
                }
                Ok(())
            }
            _ => return Errno::EINVAL.code(),
        };
        if let Err(error) = result {
            return errno(error).code();
        }
    }
    let after = satp();
    if after != before {
        return Errno::EIO.code();
    }
    if let Err(error) = SERVICE.publish_endpoint(BLOCK_DEVICE_NAME, PORT) {
        return error.code();
    }
    let state = Box::new(State {
        buffer: data.as_ptr() as usize,
        len: data.len(),
        before,
        after,
        data,
    });
    unsafe {
        *out_state = Box::into_raw(state).cast();
    }
    0
});
kcomp_sdk::kcomp_instance_destroy!(|state| {
    unsafe {
        drop(Box::from_raw(state.cast::<State>()));
    }
    0
});
