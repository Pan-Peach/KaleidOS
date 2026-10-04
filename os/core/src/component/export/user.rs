//! Checked user-task C ABI. User register/syscall policy never lives here.
use super::*;
use crate::generated::abi::UserTrap;
use crate::task::user as execution;

fn owner() -> Result<ComponentId, Errno> {
    let id = current_task_requester().ok_or(Errno::EPERM)?;
    if deny_if_failed(id).is_some() {
        return Err(Errno::EPERM);
    }
    if deny_if_isolated(id).is_some() {
        return Err(Errno::ENOTSUP);
    }
    let reg = registry::get_registry().lock();
    let record = reg.get(id).ok_or(Errno::EPERM)?;
    if !matches!(
        record.state,
        crate::component::ComponentState::Starting | crate::component::ComponentState::Ready
    ) {
        return Err(Errno::EPERM);
    }
    Ok(id)
}

pub(super) extern "C" fn kcore_user_create(entry: usize, arg: *mut (), out_task: *mut u32) -> i32 {
    with_core_critical(|| {
        if out_task.is_null() {
            return Errno::EFAULT.code();
        }
        match owner().and_then(|owner| execution::create(owner, entry, arg)) {
            Ok(id) => {
                unsafe { out_task.write_unaligned(id) };
                0
            }
            Err(error) => error.code(),
        }
    })
}

pub(super) extern "C" fn kcore_user_map(task: u32, address: u64, len: u64, permission: u32) -> i32 {
    with_core_critical(|| {
        status(owner().and_then(|owner| {
            let address = usize::try_from(address).map_err(|_| Errno::EOVERFLOW)?;
            let len = usize::try_from(len).map_err(|_| Errno::EOVERFLOW)?;
            execution::map(owner, task, address, len, permission)
        }))
    })
}

pub(super) extern "C" fn kcore_user_protect(
    task: u32,
    address: u64,
    len: u64,
    permission: u32,
) -> i32 {
    with_core_critical(|| {
        status(owner().and_then(|owner| {
            let address = usize::try_from(address).map_err(|_| Errno::EOVERFLOW)?;
            let len = usize::try_from(len).map_err(|_| Errno::EOVERFLOW)?;
            execution::protect(owner, task, address, len, permission)
        }))
    })
}

fn copy(task: u32, address: u64, buffer: *mut u8, len: usize, direction: u32) -> i32 {
    status(owner().and_then(|owner| {
        if buffer.is_null() && len != 0 {
            return Err(Errno::EFAULT);
        }
        let address = usize::try_from(address).map_err(|_| Errno::EOVERFLOW)?;
        unsafe { execution::copy(owner, task, address, buffer, len, direction) }
    }))
}

pub(super) extern "C" fn kcore_user_load(
    task: u32,
    address: u64,
    buffer: *const u8,
    len: usize,
) -> i32 {
    with_core_critical(|| copy(task, address, buffer.cast_mut(), len, 2))
}
pub(super) extern "C" fn kcore_user_read(
    task: u32,
    address: u64,
    buffer: *mut u8,
    len: usize,
) -> i32 {
    with_core_critical(|| copy(task, address, buffer, len, 0))
}
pub(super) extern "C" fn kcore_user_write(
    task: u32,
    address: u64,
    buffer: *const u8,
    len: usize,
) -> i32 {
    with_core_critical(|| copy(task, address, buffer.cast_mut(), len, 1))
}
pub(super) extern "C" fn kcore_user_prepare(task: u32, pc: u64, sp: u64) -> i32 {
    with_core_critical(|| {
        status(owner().and_then(|owner| {
            let pc = usize::try_from(pc).map_err(|_| Errno::EOVERFLOW)?;
            let sp = usize::try_from(sp).map_err(|_| Errno::EOVERFLOW)?;
            execution::prepare(owner, task, pc, sp)
        }))
    })
}
pub(super) extern "C" fn kcore_user_step(result: i64, deadline: u64, out: *mut UserTrap) -> i32 {
    with_core_critical(|| {
        if out.is_null() {
            return Errno::EFAULT.code();
        }
        match owner().and_then(|owner| execution::step(owner, result, deadline)) {
            Ok(event) => {
                unsafe { out.write_unaligned(event) };
                0
            }
            Err(error) => error.code(),
        }
    })
}
pub(super) extern "C" fn kcore_user_clone(entry: usize, arg: *mut (), out_task: *mut u32) -> i32 {
    with_core_critical(|| {
        if out_task.is_null() {
            return Errno::EFAULT.code();
        }
        match owner().and_then(|owner| execution::clone_current(owner, entry, arg)) {
            Ok(id) => {
                unsafe { out_task.write_unaligned(id) };
                0
            }
            Err(error) => error.code(),
        }
    })
}
pub(super) extern "C" fn kcore_user_replace(prepared_task: u32) -> i32 {
    with_core_critical(|| {
        status(owner().and_then(|owner| execution::replace(owner, prepared_task)))
    })
}
pub(super) extern "C" fn kcore_user_discard(task: u32) -> i32 {
    with_core_critical(|| status(owner().and_then(|owner| execution::discard(owner, task))))
}
