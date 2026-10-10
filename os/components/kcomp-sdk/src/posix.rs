//! Immutable-image process-family profile and read-only status frontend.
pub use crate::generated::posix::*;
#[cfg(feature = "alloc")]
extern crate alloc;
use crate::endpoint::{Contract, Endpoint};
use crate::{Errno, Result, abi};
pub struct PosixProcess;
impl Contract for PosixProcess {
    const ID: u64 = KCOMP_POSIX_PROCESS_CONTRACT;
    const ABI: u64 = KCOMP_POSIX_PROCESS_ABI;
    const KIND: abi::InterfaceKind = abi::InterfaceKind::Service;
}
pub struct ProcessBinding {
    endpoint: Endpoint<PosixProcess>,
}
#[derive(Debug, Clone, Copy)]
pub struct ProcessStatus {
    pub exited: bool,
    pub wait_status: u32,
    pub live: u32,
}
impl Endpoint<PosixProcess> {
    pub fn bind(&self) -> Result<ProcessBinding> {
        Ok(ProcessBinding { endpoint: *self })
    }
}
impl ProcessBinding {
    pub fn status(&self) -> Result<ProcessStatus> {
        let reply =
            crate::generated::posix_wire::status(self.endpoint.id()).map_err(invoke_error)?;
        if reply.exited > 1 {
            return Err(Errno::EPROTO);
        }
        Ok(ProcessStatus {
            exited: reply.exited == 1,
            wait_status: reply.wait_status,
            live: reply.live,
        })
    }
    /// Only an authorized consumer can close an already completed family.
    pub fn shutdown(&self) -> Result<()> {
        crate::generated::posix_wire::shutdown(self.endpoint.id()).map_err(invoke_error)
    }
}
fn invoke_error(error: crate::endpoint::InvokeError) -> Errno {
    match error {
        crate::endpoint::InvokeError::Transport(error)
        | crate::endpoint::InvokeError::Method(error) => error,
        crate::endpoint::InvokeError::InvalidReply => Errno::EPROTO,
    }
}
#[cfg(feature = "alloc")]
pub fn encode(
    images: &[(&[u8], &[u8])],
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Result<alloc::vec::Vec<u8>> {
    extern crate alloc;
    let mut result = alloc::vec::Vec::new();
    if images.is_empty() || images.len() > 8 || argv.is_empty() || argv.len() + envp.len() > 128 {
        return Err(Errno::EINVAL);
    }
    for value in [images.len(), argv.len(), envp.len(), 0] {
        result.extend_from_slice(&(value as u32).to_le_bytes());
    }
    for (name, image) in images {
        if name.is_empty()
            || name.len() > 255
            || name[0] != b'/'
            || name.contains(&0)
            || image.is_empty()
            || image.len() > 16 * 1024 * 1024
        {
            return Err(Errno::EINVAL);
        }
        result.extend_from_slice(&(name.len() as u32).to_le_bytes());
        result.extend_from_slice(&(image.len() as u32).to_le_bytes());
        result.extend_from_slice(name);
        result.extend_from_slice(image);
    }
    for string in argv.iter().chain(envp) {
        if string.len() > 4096 || string.contains(&0) {
            return Err(Errno::EINVAL);
        }
        result.extend_from_slice(&(string.len() as u32).to_le_bytes());
        result.extend_from_slice(string);
    }
    if result.len() > 16 * 1024 * 1024 {
        return Err(Errno::E2BIG);
    }
    Ok(result)
}
