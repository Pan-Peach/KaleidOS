//! CPU-only U deployment of existing Core C ABI imports. This is an adapter,
//! not a business dispatcher: Core never parses an Echo/Block/VFS method.
use super::*;
use crate::component::access;

pub(crate) fn dispatch(name: &[u8], a: [usize; 8]) -> usize {
    let result = dispatch_inner(name, a);
    result.unwrap_or_else(|e| e.code() as usize)
}
fn dispatch_inner(name: &[u8], a: [usize; 8]) -> Result<usize, Errno> {
    let owner = RequestContext::ambient().ok_or(Errno::EPERM)?.component;
    let result = match name {
        b"kcore_now" => return Ok(kcore_now() as usize),
        b"kcore_timebase_hz" => return Ok(kcore_timebase_hz() as usize),
        b"kcore_cpu_current" => return Ok(kcore_cpu_current() as usize),
        b"kcore_console_write_byte" => {
            kcore_console_write_byte(a[0] as u8);
            0
        }
        b"kcore_component_current" => {
            access::put(a[0] as *mut u32, owner.raw())?;
            0
        }
        b"kcore_log_line" => {
            if a[1] > 1024 {
                return Err(Errno::EMSGSIZE);
            }
            let mut bytes = [0; 1024];
            access::read(a[0], &mut bytes[..a[1]])?;
            kcore_log_line(bytes.as_ptr(), a[1])
        }
        b"kcore_task_create" => kcore_task_create(a[0], a[1] as *mut (), a[2] as *mut u32),
        b"kcore_task_start" => kcore_task_start(a[0] as u32),
        b"kcore_task_start_on" => kcore_task_start_on(a[0] as u32, a[1] as u32),
        b"kcore_task_stop_requested" => kcore_task_stop_requested(),
        b"kcore_task_yield" => kcore_task_yield(),
        b"kcore_memory_acquire" => {
            let registry = registry::get_registry().lock();
            if !registry.may_run(owner) {
                return Err(Errno::EPERM);
            }
            let space = registry
                .get(owner)
                .and_then(|r| r.address_space)
                .ok_or(Errno::EPERM)?;
            {
                let buffers = access::Pinned::new(Some(space), true)?;
                buffers.validate(a[2], core::mem::size_of::<MemoryView>(), true)?;
            }
            if a[0] == 0 || a[1] == 0 || !a[1].is_power_of_two() {
                return Err(Errno::EINVAL);
            }
            let view = crate::component::backing::acquire_user(space, a[0], a[1])?;
            access::Pinned::new(Some(space), true)?.put(a[2] as *mut MemoryView, view)?;
            0
        }
        b"kcore_memory_release" => {
            let mut bytes = [0; core::mem::size_of::<MemoryView>()];
            access::read(a[0], &mut bytes)?;
            let view = unsafe { bytes.as_ptr().cast::<MemoryView>().read_unaligned() };
            if view.kind != KCORE_MEMORY_VIEW_LOCAL_VA || view.reserved != 0 {
                return Err(Errno::EINVAL);
            }
            let registry = registry::get_registry().lock();
            let space = registry
                .get(owner)
                .and_then(|r| r.address_space)
                .ok_or(Errno::EPERM)?;
            crate::component::backing::release(space, view)?;
            0
        }
        b"kcore_endpoint_validate" => {
            kcore_endpoint_validate(a[0] as u64, a[1] as u64, a[2] as u64)
        }
        b"kcore_endpoint_publish" | b"kcore_endpoint_lookup" => {
            let (address, len) = if name == b"kcore_endpoint_publish" {
                (a[0], a[1])
            } else {
                (a[1], a[2])
            };
            if len == 0 || len > 64 {
                return Err(Errno::EINVAL);
            }
            let mut bytes = [0; 64];
            access::read(address, &mut bytes[..len])?;
            if name == b"kcore_endpoint_publish" {
                if a[5] != 0 || a[6] != 0 || a[7] != 0 {
                    return Err(Errno::ENOTSUP);
                }
                kcore_endpoint_publish(
                    bytes.as_ptr(),
                    len,
                    a[2] as u64,
                    a[3] as u32,
                    a[4] as u64,
                    0,
                    core::ptr::null(),
                    core::ptr::null_mut(),
                )
            } else {
                access::validate(a[4], 8, true)?;
                let mut endpoint = 0;
                let code = kcore_endpoint_lookup(
                    a[0] as u32,
                    bytes.as_ptr(),
                    len,
                    a[3] as u64,
                    &mut endpoint,
                );
                if code == 0 {
                    access::put(a[4] as *mut u64, endpoint)?;
                }
                code
            }
        }
        b"kcore_ipc_listen" => kcore_ipc_listen(a[0] as u64),
        b"kcore_ipc_grant" => kcore_ipc_grant(a[0] as u64, a[1] as u32),
        b"kcore_ipc_submit" => {
            kcore_ipc_submit(a[0] as u64, a[1] as *const u8, a[2], a[3] as *mut u64)
        }
        b"kcore_ipc_receive" => kcore_ipc_receive(
            a[0] as u64,
            a[1] as *mut u8,
            a[2],
            a[3] as *mut u64,
            a[4] as *mut u32,
            a[5] as *mut u32,
            a[6] as *mut usize,
        ),
        b"kcore_ipc_reply" => kcore_ipc_reply(a[0] as u64, a[1] as *const u8, a[2]),
        b"kcore_ipc_collect" => kcore_ipc_collect(
            a[0] as u64,
            a[1] as *mut u8,
            a[2],
            a[3] as *mut usize,
            a[4] as *mut i32,
        ),
        b"kcore_ipc_wait" => kcore_ipc_wait(a[0] as u64, a[1] as u64),
        b"kcore_ipc_cancel" => kcore_ipc_cancel(a[0] as u64),
        b"kcore_ipc_close" => kcore_ipc_close(a[0] as u64),
        _ => return Err(Errno::ENOSYS),
    };
    Ok(result as usize)
}
