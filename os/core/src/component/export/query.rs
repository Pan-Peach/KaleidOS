//! Console 与只读值查询。锁只覆盖取值 / 拷贝，不跨组件调用。

use super::*;
use crate::generated::abi::{
    ComponentInfo, DeviceClaimState, DeviceInfo, DeviceSpaceKind, EndpointInfo,
};

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

fn device_snapshot(
    id: u32,
    descriptor: &machine::DeviceDescriptor,
    table: &device::DeviceTable,
) -> Result<DeviceInfo, Errno> {
    let identity = machine::DeviceId::from_raw(id);
    if id as usize >= table.len() {
        return Err(Errno::ENOENT);
    }
    let owner = table.owner(identity);
    let state = if table.is_quarantined(identity) {
        DeviceClaimState::Quarantined
    } else if owner.is_some() {
        DeviceClaimState::Claimed
    } else {
        DeviceClaimState::Unclaimed
    };
    let (kind, base, size) = match descriptor.spaces.first() {
        Some(&machine::IoSpace::Mmio { base, size }) => (DeviceSpaceKind::Mmio, base, size),
        Some(&machine::IoSpace::Pio { base, size }) => (DeviceSpaceKind::Pio, base, size),
        None => (DeviceSpaceKind::None, 0, 0),
    };
    let line = descriptor.interrupts.first().and_then(|irq| irq.line);
    let count = |len| u32::try_from(len).map_err(|_| Errno::EOVERFLOW);
    Ok(DeviceInfo {
        id,
        state: state as u32,
        owner: owner.map_or(0, |owner| u64::from(owner.raw())),
        compatible_len: count(descriptor.compatibles.first().map_or(0, |s| s.len()))?,
        compatible_count: count(descriptor.compatibles.len())?,
        space_count: count(descriptor.spaces.len())?,
        interrupt_count: count(descriptor.interrupts.len())?,
        space_kind: kind as u32,
        irq_known: u32::from(line.is_some()),
        base: base as u64,
        size: size as u64,
        irq_line: line.unwrap_or(0),
        reserved: 0,
    })
}

fn copy_device(
    info: DeviceInfo,
    source: &[u8],
    out: *mut DeviceInfo,
    compatible: *mut u8,
    capacity: usize,
) -> Result<(), Errno> {
    if out.is_null() {
        return Err(Errno::EFAULT);
    }
    // Validate every output before writing either one; no partial failure reply.
    copy_name(source, compatible, capacity)?;
    unsafe { out.write_unaligned(info) };
    Ok(())
}

pub(super) extern "C" fn kcore_device_info(
    id: u32,
    out: *mut DeviceInfo,
    compatible: *mut u8,
    capacity: usize,
) -> i32 {
    with_core_critical(|| {
        if out.is_null() || compatible.is_null() {
            return Errno::EFAULT.code();
        }
        let Some(machine) = machine::committed() else {
            return Errno::ENODEV.code();
        };
        let Some(descriptor) = machine.devices.get(id as usize) else {
            return Errno::ENOENT.code();
        };
        let info = match device_snapshot(id, descriptor, &device::get_table().lock()) {
            Ok(info) => info,
            Err(error) => return error.code(),
        };
        let source = descriptor
            .compatibles
            .first()
            .map_or(b"".as_slice(), |s| s.as_bytes());
        match copy_device(info, source, out, compatible, capacity) {
            Ok(()) => 0,
            Err(error) => error.code(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn descriptor() -> machine::DeviceDescriptor {
        use alloc::{boxed::Box, vec};
        machine::DeviceDescriptor {
            spaces: vec![
                machine::IoSpace::Pio {
                    base: 0x3f8,
                    size: 8,
                },
                machine::IoSpace::Mmio {
                    base: 0x1000,
                    size: 0x2000,
                },
            ]
            .into_boxed_slice(),
            interrupts: vec![
                machine::InterruptResource {
                    specifier: machine::InterruptSpecifier::Isa { line: 0 },
                    line: Some(0),
                },
                machine::InterruptResource {
                    specifier: machine::InterruptSpecifier::Isa { line: 1 },
                    line: None,
                },
            ]
            .into_boxed_slice(),
            compatibles: vec![Box::<str>::from("ns16550a"), Box::<str>::from("serial")]
                .into_boxed_slice(),
        }
    }

    #[test]
    fn device_snapshot_preserves_primary_resources_counts_and_full_identity() {
        let mut table = device::DeviceTable::new(261);
        let identity = machine::DeviceId::from_raw(260);
        table.claim(ComponentId::from_raw(0), identity).unwrap();
        let descriptor = descriptor();
        let info = device_snapshot(260, &descriptor, &table).unwrap();
        assert_eq!(
            (info.id, info.state, info.owner),
            (260, DeviceClaimState::Claimed as u32, 0)
        );
        assert_eq!((info.compatible_len, info.compatible_count), (8, 2));
        assert_eq!(
            (info.space_kind, info.base, info.size, info.space_count),
            (DeviceSpaceKind::Pio as u32, 0x3f8, 8, 2)
        );
        assert_eq!(
            (info.irq_known, info.irq_line, info.interrupt_count),
            (1, 0, 2)
        );
        assert_eq!(
            device_snapshot(261, &descriptor, &table),
            Err(Errno::ENOENT)
        );
        let mut unresolved = descriptor.clone();
        unresolved.interrupts[0].line = None;
        let info = device_snapshot(260, &unresolved, &table).unwrap();
        assert_eq!(
            (info.irq_known, info.irq_line, info.interrupt_count),
            (0, 0, 2)
        );
        let empty = device_snapshot(0, &machine::DeviceDescriptor::empty(), &table).unwrap();
        assert_eq!(
            (
                empty.space_kind,
                empty.base,
                empty.size,
                empty.interrupt_count,
                empty.compatible_count
            ),
            (0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn device_observation_tracks_owner_release_and_quarantine_without_claiming() {
        let mut table = device::DeviceTable::new(1);
        let id = machine::DeviceId::from_raw(0);
        let owner = ComponentId::from_raw(52);
        let descriptor = descriptor();
        assert_eq!(
            device_snapshot(0, &descriptor, &table).unwrap().state,
            DeviceClaimState::Unclaimed as u32
        );
        table.claim(owner, id).unwrap();
        let claimed = device_snapshot(0, &descriptor, &table).unwrap();
        assert_eq!(
            (claimed.state, claimed.owner),
            (DeviceClaimState::Claimed as u32, 52)
        );
        assert_eq!(device_snapshot(0, &descriptor, &table).unwrap(), claimed);
        assert_eq!(table.owner(id), Some(owner));
        table.release(owner, id).unwrap();
        let released = device_snapshot(0, &descriptor, &table).unwrap();
        assert_eq!(
            (released.state, released.owner),
            (DeviceClaimState::Unclaimed as u32, 0)
        );
        table.claim(owner, id).unwrap();
        table.quarantine_owner(owner);
        let failed = device_snapshot(0, &descriptor, &table).unwrap();
        assert_eq!(
            (failed.state, failed.owner),
            (DeviceClaimState::Quarantined as u32, 0)
        );
        assert!(table.is_quarantined(id));
    }

    #[test]
    fn device_copy_checks_both_outputs_before_writing_and_accepts_unaligned_output() {
        let info = device_snapshot(0, &descriptor(), &device::DeviceTable::new(1)).unwrap();
        let mut output = [0xaa; 65];
        let out = unsafe { output.as_mut_ptr().add(1).cast::<DeviceInfo>() };
        let mut compatible = [0xbb; 8];
        assert_eq!(
            copy_device(info, b"ns16550a", out, compatible.as_mut_ptr(), 7),
            Err(Errno::ENOBUFS)
        );
        assert_eq!(
            copy_device(info, b"ns16550a", out, core::ptr::null_mut(), 8),
            Err(Errno::EFAULT)
        );
        assert_eq!(
            copy_device(
                info,
                b"ns16550a",
                core::ptr::null_mut(),
                compatible.as_mut_ptr(),
                8
            ),
            Err(Errno::EFAULT)
        );
        assert_eq!(output, [0xaa; 65]);
        assert_eq!(compatible, [0xbb; 8]);
        assert_eq!(
            copy_device(info, b"ns16550a", out, compatible.as_mut_ptr(), 8),
            Ok(())
        );
        assert_eq!(unsafe { out.read_unaligned() }, info);
        assert_eq!(&compatible, b"ns16550a");
        assert_eq!(output[0], 0xaa);
    }

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
