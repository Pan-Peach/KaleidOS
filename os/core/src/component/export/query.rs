//! Console 与只读值查询。锁只覆盖取值 / 拷贝，不跨组件调用。

use super::*;
use crate::generated::abi::{ComponentInfo, EndpointInfo};

pub(super) extern "C" fn kcore_console_read_byte() -> i32 {
    with_core_critical(|| match ConsoleImpl::getc() {
        Some(byte) => i32::from(byte),
        None => {
            crate::print::idle_wait();
            Errno::EAGAIN.code()
        }
    })
}

// SAFETY contract: caller supplies capacity writable bytes. No NUL / truncation.
fn copy_name(source: &[u8], target: *mut u8, capacity: usize) -> Result<(), Errno> {
    if target.is_null() {
        return Err(Errno::EFAULT);
    }
    if capacity < source.len() {
        return Err(Errno::ENOBUFS);
    }
    unsafe { core::ptr::copy_nonoverlapping(source.as_ptr(), target, source.len()) };
    Ok(())
}

pub(super) extern "C" fn kcore_component_nth(
    ordinal: u32,
    out: *mut ComponentInfo,
    name: *mut u8,
    capacity: usize,
) -> i32 {
    with_core_critical(|| {
        if out.is_null() {
            return Errno::EFAULT.code();
        }
        let registry = registry::get_registry().lock();
        let Some(record) = registry.iter().nth(ordinal as usize) else {
            return Errno::ENOENT.code();
        };
        if let Err(error) = copy_name(&record.name, name, capacity) {
            return error.code();
        }
        use crate::component::ComponentState as State;
        use crate::generated::abi::{ComponentState as WireState, ExecutionDomain as WireDomain};
        let state = match record.state {
            State::Declared => WireState::Declared,
            State::Resolved => WireState::Resolved,
            State::Starting => WireState::Starting,
            State::Ready => WireState::Ready,
            State::Stopping => WireState::Stopping,
            State::Stopped => WireState::Stopped,
            State::Failed => WireState::Failed,
        };
        let domain = match record.execution_domain {
            ExecutionDomain::KernelNative => WireDomain::KernelNative,
            ExecutionDomain::IsolatedNative => WireDomain::IsolatedNative,
            ExecutionDomain::SandboxedNative => WireDomain::SandboxedNative,
        };
        let info = ComponentInfo {
            id: record.id.raw(),
            state: state as u32,
            domain: domain as u32,
            name_len: record.name.len() as u32,
        };
        // SAFETY: caller owns output, arbitrary alignment accepted.
        unsafe { out.write_unaligned(info) };
        0
    })
}

pub(super) extern "C" fn kcore_endpoint_nth(
    ordinal: u32,
    out: *mut EndpointInfo,
    name: *mut u8,
    capacity: usize,
) -> i32 {
    with_core_critical(|| {
        if out.is_null() {
            return Errno::EFAULT.code();
        }
        let endpoints = endpoint::get_endpoints().lock();
        let Some((info, source)) = endpoints.observation(ordinal as usize) else {
            return Errno::ENOENT.code();
        };
        if let Err(error) = copy_name(source, name, capacity) {
            return error.code();
        }
        unsafe { out.write_unaligned(info) };
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_complete_or_not_written() {
        let mut bytes = [0xAA; 4];
        assert_eq!(
            copy_name(b"hello", bytes.as_mut_ptr(), bytes.len()),
            Err(Errno::ENOBUFS)
        );
        assert_eq!(bytes, [0xAA; 4]);
        assert_eq!(copy_name(b"abcd", bytes.as_mut_ptr(), bytes.len()), Ok(()));
        assert_eq!(&bytes, b"abcd");
        assert_eq!(
            copy_name(b"x", core::ptr::null_mut(), 1),
            Err(Errno::EFAULT)
        );
    }
}
