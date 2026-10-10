//! Typed IPC filesystem binding. Wire codecs and dispatch come from the schema.
use crate::{
    Errno,
    endpoint::{Endpoint, InvokeError},
    filesystem::FileSystem,
    generated::{filesystem::*, filesystem_wire as wire},
};
use core::ffi::CStr;
pub struct FileSystemBinding {
    endpoint: u64,
}
impl Endpoint<FileSystem> {
    pub fn bind(&self) -> Result<FileSystemBinding, InvokeError> {
        FileSystemBinding::connect(self.id())
    }
}
impl FileSystemBinding {
    pub fn connect(endpoint: u64) -> Result<Self, InvokeError> {
        use crate::{abi, endpoint::Contract};
        let (mut mechanism, mut api, mut ctx) = (0, 0, 0);
        let rc = unsafe {
            abi::kcore_endpoint_bind(
                endpoint,
                FileSystem::ID,
                FileSystem::ABI,
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
    pub fn mount(&self) -> Result<(), InvokeError> {
        wire::mount(self.endpoint)
    }
    pub fn unmount(&self) -> Result<(), InvokeError> {
        wire::unmount(self.endpoint)
    }
    pub fn shutdown(&self) -> Result<(), InvokeError> {
        wire::shutdown(self.endpoint)
    }
    pub fn open(&self, path: &CStr, flags: u32) -> Result<u64, InvokeError> {
        let handle = wire::open(self.endpoint, flags, path.to_bytes_with_nul())?;
        identity(handle)
    }
    pub fn close(&self, handle: u64) -> Result<(), InvokeError> {
        wire::close(self.endpoint, handle)
    }
    pub fn read(&self, handle: u64, buf: &mut [u8]) -> Result<usize, InvokeError> {
        let mut scratch = [0; 512];
        let capacity = buf.len().min(scratch.len());
        let actual = wire::read(self.endpoint, handle, &mut scratch[..capacity])?;
        let actual = usize::try_from(actual).map_err(|_| InvokeError::InvalidReply)?;
        if actual > capacity {
            return Err(InvokeError::InvalidReply);
        }
        buf[..actual].copy_from_slice(&scratch[..actual]);
        Ok(actual)
    }
    pub fn root(&self) -> Result<u64, InvokeError> {
        identity(wire::root(self.endpoint)?)
    }
    pub fn lookup(&self, parent: u64, name: &[u8], encoding: u32) -> Result<u64, InvokeError> {
        if name.is_empty() || name.len() > KCOMP_FILESYSTEM_NAME_MAX {
            return Err(InvokeError::Method(Errno::EINVAL));
        }
        identity(wire::lookup(self.endpoint, parent, encoding, name)?)
    }
    pub fn node_info(&self, node: u64) -> Result<u32, InvokeError> {
        wire::node_info(self.endpoint, node)
    }
}
fn identity(id: u64) -> Result<u64, InvokeError> {
    if id == 0 {
        Err(InvokeError::InvalidReply)
    } else {
        Ok(id)
    }
}
