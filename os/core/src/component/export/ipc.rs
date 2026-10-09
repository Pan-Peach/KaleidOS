//! KernelNative Task IPC ABI. Private-domain imports remain explicitly absent.
use super::*;
use crate::component::{containment, exchange};
use crate::irq::IrqSaveGuard;

fn caller() -> Result<(ComponentId, TaskId), Errno> {
    if containment::task_switch_forbidden() {
        return Err(Errno::EINVAL);
    }
    let ctx = RequestContext::ambient().ok_or(Errno::EPERM)?;
    let task = ctx.task.ok_or(Errno::EPERM)?;
    if crate::sched::current_task() != Some(task) {
        return Err(Errno::EPERM);
    }
    Ok((ctx.component, task))
}
fn transaction<T>(
    endpoint: Option<EndpointId>,
    own: bool,
    f: impl FnOnce(ComponentId, TaskId, &mut exchange::Exchange) -> Result<T, Errno>,
) -> Result<T, Errno> {
    let (owner, task) = caller()?;
    let _irq = IrqSaveGuard::new();
    let registry = registry::get_registry().lock();
    let record = registry.get(owner).ok_or(Errno::EPERM)?;
    if !registry.may_run(owner) {
        return Err(Errno::EPERM);
    }
    if record.execution_domain != ExecutionDomain::KernelNative {
        return Err(Errno::ENOTSUP);
    }
    let endpoints = endpoint::get_endpoints().lock();
    if let Some(id) = endpoint {
        let provider = endpoints.resolve(&registry, id).map_err(Errno::from)?;
        if own && provider.owner != owner {
            return Err(Errno::EACCES);
        }
        if endpoint::instance_domain(&registry, provider.owner) != ExecutionDomain::KernelNative {
            return Err(Errno::ENOTSUP);
        }
    }
    let mut state = exchange::get().lock();
    // The ambient principal cannot claim another Task, including under nested create.
    let table = task::get_task_table().lock();
    if table.get(task).is_none_or(|t| {
        t.owner() != owner || t.state() != TaskState::Running(crate::smp::current_cpu())
    }) {
        return Err(Errno::EPERM);
    }
    drop(table);
    f(owner, task, &mut state)
}
fn buffer(ptr: *const u8, len: usize) -> Result<(), Errno> {
    if len > exchange::MESSAGE_MAX {
        return Err(Errno::EMSGSIZE);
    }
    if len != 0 && (ptr.is_null() || (ptr as usize).checked_add(len).is_none()) {
        return Err(Errno::EFAULT);
    }
    Ok(())
}
fn output<T>(ptr: *mut T) -> Result<(), Errno> {
    if ptr.is_null()
        || !(ptr as usize).is_multiple_of(core::mem::align_of::<T>())
        || (ptr as usize)
            .checked_add(core::mem::size_of::<T>())
            .is_none()
    {
        return Err(Errno::EFAULT);
    }
    Ok(())
}
unsafe fn input<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(ptr, len) }
    }
}
unsafe fn out<'a>(ptr: *mut u8, len: usize) -> &'a mut [u8] {
    if len == 0 {
        &mut []
    } else {
        unsafe { core::slice::from_raw_parts_mut(ptr, len) }
    }
}

pub(super) extern "C" fn kcore_ipc_listen(endpoint: u64) -> i32 {
    with_core_critical(|| {
        status(transaction(
            Some(EndpointId::from_raw(endpoint)),
            true,
            |owner, task, state| state.listen(owner, task, EndpointId::from_raw(endpoint)),
        ))
    })
}
pub(super) extern "C" fn kcore_ipc_grant(endpoint: u64, consumer: u32) -> i32 {
    with_core_critical(|| {
        if containment::scheduling_forbidden() {
            return Errno::EINVAL.code();
        }
        let Some(ctx) = RequestContext::ambient() else {
            return Errno::EPERM.code();
        };
        let _irq = IrqSaveGuard::new();
        let registry = registry::get_registry().lock();
        if !registry.may_run(ctx.component) {
            return Errno::EPERM.code();
        }
        if endpoint::instance_domain(&registry, ctx.component) != ExecutionDomain::KernelNative {
            return Errno::ENOTSUP.code();
        }
        if !registry.may_run(ComponentId::from_raw(consumer)) {
            return Errno::ESRCH.code();
        }
        let endpoints = endpoint::get_endpoints().lock();
        let id = EndpointId::from_raw(endpoint);
        let provider = match endpoints.resolve(&registry, id) {
            Ok(p) => p.owner,
            Err(e) => return Errno::from(e).code(),
        };
        if ctx.component != provider && !registry.created_by(ctx.component, provider) {
            return Errno::EACCES.code();
        }
        status(
            exchange::get()
                .lock()
                .grant(provider, id, ComponentId::from_raw(consumer)),
        )
    })
}
pub(super) extern "C" fn kcore_ipc_submit(
    endpoint: u64,
    bytes: *const u8,
    len: usize,
    request: *mut u64,
) -> i32 {
    with_core_critical(|| {
        if let Err(e) = buffer(bytes, len).and_then(|_| output(request)) {
            return e.code();
        }
        let result = transaction(
            Some(EndpointId::from_raw(endpoint)),
            false,
            |owner, task, state| {
                state.submit(owner, task, EndpointId::from_raw(endpoint), unsafe {
                    input(bytes, len)
                })
            },
        );
        match result {
            Ok((id, wake)) => {
                unsafe { request.write(id) };
                exchange::wake(wake);
                0
            }
            Err(e) => e.code(),
        }
    })
}
pub(super) extern "C" fn kcore_ipc_receive(
    endpoint: u64,
    bytes: *mut u8,
    capacity: usize,
    request: *mut u64,
    consumer: *mut u32,
    consumer_task: *mut u32,
    length: *mut usize,
) -> i32 {
    with_core_critical(|| {
        if let Err(e) = buffer(bytes, capacity)
            .and_then(|_| output(request))
            .and_then(|_| output(consumer))
            .and_then(|_| output(consumer_task))
            .and_then(|_| output(length))
        {
            return e.code();
        }
        match transaction(
            Some(EndpointId::from_raw(endpoint)),
            true,
            |_, task, state| {
                state.receive(task, EndpointId::from_raw(endpoint), unsafe {
                    out(bytes, capacity)
                })
            },
        ) {
            Ok((id, caller, task, len)) => {
                unsafe {
                    request.write(id);
                    consumer.write(caller.raw());
                    consumer_task.write(task.raw());
                    length.write(len);
                }
                0
            }
            Err(e) => e.code(),
        }
    })
}
pub(super) extern "C" fn kcore_ipc_reply(request: u64, bytes: *const u8, len: usize) -> i32 {
    with_core_critical(|| {
        if let Err(e) = buffer(bytes, len) {
            return e.code();
        }
        match transaction(None, false, |_, task, state| {
            state.reply(task, request, unsafe { input(bytes, len) })
        }) {
            Ok(wake) => {
                exchange::wake(wake);
                0
            }
            Err(e) => e.code(),
        }
    })
}
pub(super) extern "C" fn kcore_ipc_collect(
    request: u64,
    bytes: *mut u8,
    capacity: usize,
    length: *mut usize,
    completion: *mut i32,
) -> i32 {
    with_core_critical(|| {
        if let Err(e) = buffer(bytes, capacity)
            .and_then(|_| output(length))
            .and_then(|_| output(completion))
        {
            return e.code();
        }
        match transaction(None, false, |_, task, state| {
            state.collect(task, request, unsafe { out(bytes, capacity) })
        }) {
            Ok((status, len)) => {
                unsafe {
                    length.write(len);
                    completion.write(status);
                }
                0
            }
            Err(e) => e.code(),
        }
    })
}
pub(super) extern "C" fn kcore_ipc_wait(endpoint: u64, request: u64) -> i32 {
    with_core_critical(|| {
        match transaction(
            (request == 0).then_some(EndpointId::from_raw(endpoint)),
            true,
            |_, task, state| state.wait(task, EndpointId::from_raw(endpoint), request),
        ) {
            Ok(true) => status(sched::park_current()),
            Ok(false) => 0,
            Err(e) => e.code(),
        }
    })
}
pub(super) extern "C" fn kcore_ipc_cancel(request: u64) -> i32 {
    with_core_critical(|| {
        match transaction(None, false, |_, task, state| state.cancel(task, request)) {
            Ok(wake) => {
                exchange::wake(wake);
                0
            }
            Err(e) => e.code(),
        }
    })
}
pub(super) extern "C" fn kcore_ipc_close(endpoint: u64) -> i32 {
    with_core_critical(|| {
        let (owner, _) = match caller() {
            Ok(c) => c,
            Err(e) => return e.code(),
        };
        let _irq = IrqSaveGuard::new();
        let registry = registry::get_registry().lock();
        let mut endpoints = endpoint::get_endpoints().lock();
        let id = EndpointId::from_raw(endpoint);
        let record = match endpoints.resolve(&registry, id) {
            Ok(r) => r,
            Err(e) => return Errno::from(e).code(),
        };
        if record.owner != owner {
            return Errno::EACCES.code();
        }
        if endpoint::instance_domain(&registry, owner) != ExecutionDomain::KernelNative {
            return Errno::ENOTSUP.code();
        }
        endpoints.invalidate(id);
        let wakes = exchange::get().lock().close(id);
        drop(endpoints);
        drop(registry);
        exchange::wake(wakes.into_iter().flatten());
        0
    })
}
