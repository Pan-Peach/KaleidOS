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
enum Backend {
    Direct(*const PosixProcessApi, *mut ()),
    Gate(u64),
}
pub struct ProcessBinding {
    backend: Backend,
}
#[derive(Debug, Clone, Copy)]
pub struct ProcessStatus {
    pub exited: bool,
    pub wait_status: u32,
    pub live: u32,
}
impl Endpoint<PosixProcess> {
    pub fn bind(&self) -> Result<ProcessBinding> {
        let (mut mechanism, mut api, mut ctx) = (0, 0, 0);
        let code = unsafe {
            abi::kcore_endpoint_bind(
                self.id(),
                PosixProcess::ID,
                PosixProcess::ABI,
                &mut mechanism,
                &mut api,
                &mut ctx,
            )
        };
        if code != 0 {
            return Err(Errno::from_code(code));
        }
        let backend = match mechanism {
            abi::KCORE_ENDPOINT_MECHANISM_DIRECT if api != 0 => {
                Backend::Direct(api as *const _, ctx as *mut ())
            }
            abi::KCORE_ENDPOINT_MECHANISM_GATE => Backend::Gate(self.id()),
            _ => return Err(Errno::EIO),
        };
        Ok(ProcessBinding { backend })
    }
}
impl ProcessBinding {
    pub fn status(&self) -> Result<ProcessStatus> {
        let (mut exited, mut wait_status, mut live) = (0, 0, 0);
        let code = match self.backend {
            Backend::Direct(api, ctx) => unsafe {
                ((*api).status)(ctx, &mut exited, &mut wait_status, &mut live)
            },
            Backend::Gate(endpoint) => {
                let mut reply = [0; 12];
                let code = crate::call::endpoint_call(endpoint, 0, &[], &[], &mut reply)?;
                if code == 0 {
                    exited = u32::from_le_bytes(reply[..4].try_into().unwrap());
                    wait_status = u32::from_le_bytes(reply[4..8].try_into().unwrap());
                    live = u32::from_le_bytes(reply[8..].try_into().unwrap());
                }
                code
            }
        };
        if code != 0 {
            return Err(Errno::from_code(code));
        }
        if exited > 1 {
            return Err(Errno::EIO);
        }
        Ok(ProcessStatus {
            exited: exited == 1,
            wait_status,
            live,
        })
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
