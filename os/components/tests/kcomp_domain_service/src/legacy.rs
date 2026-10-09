//! Temporary test-only client for the synchronous execution-domain matrix.
//! Isolated images cannot import IPC until persistent private-domain Tasks and
//! checked cross-AS copies exist. Keep this fixture independent of the SDK's
//! mixed backend so linker reachability cannot silently expand its import set.
use kcomp_sdk::{
    Errno, abi,
    block::{BlockDevice, BlockDeviceApi},
    call,
    endpoint::{Contract, Endpoint, InvokeError},
    generated::block::{
        KCOMP_BLOCK_METHOD_CAPACITY, KCOMP_BLOCK_METHOD_READ, KCOMP_BLOCK_METHOD_WRITE,
    },
};

pub trait LegacyBind {
    fn legacy_bind(&self) -> Result<Binding, InvokeError>;
}

pub struct Binding {
    endpoint: u64,
    api: Option<&'static BlockDeviceApi>,
    ctx: *mut (),
}

impl LegacyBind for Endpoint<BlockDevice> {
    fn legacy_bind(&self) -> Result<Binding, InvokeError> {
        let (mut mechanism, mut api, mut ctx) = (0, 0, 0);
        let rc = unsafe {
            abi::kcore_endpoint_bind(
                self.id(),
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
        let table = match mechanism {
            abi::KCORE_ENDPOINT_MECHANISM_DIRECT if api != 0 => {
                // SAFETY: exact ABI checked by Core; native export backing is resident.
                Some(unsafe { &*(api as *const BlockDeviceApi) })
            }
            abi::KCORE_ENDPOINT_MECHANISM_GATE => None,
            _ => return Err(InvokeError::InvalidReply),
        };
        Ok(Binding {
            endpoint: self.id(),
            api: table,
            ctx: ctx as *mut (),
        })
    }
}

fn status(rc: i32) -> Result<(), InvokeError> {
    match rc {
        0 => Ok(()),
        rc if rc < 0 => Err(InvokeError::Method(Errno::from_code(rc))),
        _ => Err(InvokeError::InvalidReply),
    }
}

impl Binding {
    fn gate(
        &self,
        method: u32,
        args: &[u8],
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), InvokeError> {
        status(
            call::endpoint_call(self.endpoint, method, args, input, output)
                .map_err(InvokeError::Transport)?,
        )
    }

    pub fn capacity_sectors(&self) -> Result<u64, InvokeError> {
        if let Some(api) = self.api {
            // SAFETY: Core supplied the exact table and opaque context.
            return Ok(unsafe { (api.capacity_sectors)(self.ctx) });
        }
        let mut output = [0; 8];
        self.gate(KCOMP_BLOCK_METHOD_CAPACITY, &[], &[], &mut output)?;
        Ok(u64::from_le_bytes(output))
    }

    pub fn read(&self, lba: u64, output: &mut [u8]) -> Result<(), InvokeError> {
        if output.is_empty() || !output.len().is_multiple_of(512) {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        if let Some(api) = self.api {
            // SAFETY: the output slice is borrowed only for this synchronous call.
            status(unsafe { (api.read)(self.ctx, lba, output.as_mut_ptr(), output.len()) })
        } else {
            self.gate(KCOMP_BLOCK_METHOD_READ, &lba.to_le_bytes(), &[], output)
        }
    }

    pub fn write(&self, lba: u64, input: &[u8]) -> Result<(), InvokeError> {
        if input.is_empty() || !input.len().is_multiple_of(512) {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        if let Some(api) = self.api {
            // SAFETY: the input slice is borrowed only for this synchronous call.
            status(unsafe { (api.write)(self.ctx, lba, input.as_ptr(), input.len()) })
        } else {
            self.gate(KCOMP_BLOCK_METHOD_WRITE, &lba.to_le_bytes(), input, &mut [])
        }
    }
}
