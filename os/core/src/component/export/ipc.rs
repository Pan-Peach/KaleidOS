//! One Exchange across execution domains. Copy buffers while the AS is pinned;
//! scheduling only happens after every registry/endpoint/AS/Exchange lock drops.
use super::*;
use crate::component::{access, containment, exchange, isolated_api};
use crate::irq::IrqSaveGuard;

fn core_call(f: impl FnOnce() -> i32 + 'static) -> i32 {
    if isolated_api::active() {
        isolated_api::on_core(move || with_core_critical(f)).unwrap_or_else(Errno::code)
    } else {
        with_core_critical(f)
    }
}
fn caller() -> Result<(ComponentId, TaskId), Errno> {
    if containment::task_switch_forbidden() {
        return Err(Errno::EINVAL);
    }
    let ctx = RequestContext::ambient().ok_or(Errno::EPERM)?;
    let task = ctx.task.ok_or(Errno::EPERM)?;
    if sched::current_task() != Some(task) {
        return Err(Errno::EPERM);
    }
    Ok((ctx.component, task))
}
fn transaction<T>(
    endpoint: Option<EndpointId>,
    own: bool,
    f: impl FnOnce(
        ComponentId,
        TaskId,
        &mut exchange::Exchange,
        &mut access::Pinned,
    ) -> Result<T, Errno>,
) -> Result<T, Errno> {
    let (owner, task) = caller()?;
    let _irq = IrqSaveGuard::new();
    let registry = registry::get_registry().lock();
    let record = registry.get(owner).ok_or(Errno::EPERM)?;
    if !registry.may_run(owner) {
        return Err(Errno::EPERM);
    }
    let endpoints = endpoint::get_endpoints().lock();
    if let Some(id) = endpoint {
        let provider = endpoints.resolve(&registry, id).map_err(Errno::from)?;
        if own && provider.owner != owner {
            return Err(Errno::EACCES);
        }
    }
    let table = task::get_task_table().lock();
    if table.get(task).is_none_or(|t| {
        t.owner() != owner || t.state() != TaskState::Running(crate::smp::current_cpu())
    }) {
        return Err(Errno::EPERM);
    }
    drop(table);
    let mut buffers = access::Pinned::new(
        record.address_space,
        record.execution_domain == ExecutionDomain::SandboxedNative,
    )?;
    let mut state = exchange::get().lock();

    f(owner, task, &mut state, &mut buffers)
}
fn buffer(ptr: usize, len: usize) -> Result<(), Errno> {
    if len > exchange::MESSAGE_MAX {
        return Err(Errno::EMSGSIZE);
    }
    if len != 0 && (ptr == 0 || ptr.checked_add(len).is_none()) {
        return Err(Errno::EFAULT);
    }
    Ok(())
}
fn output<T>(buffers: &access::Pinned, ptr: *mut T) -> Result<(), Errno> {
    if ptr.is_null() || !(ptr as usize).is_multiple_of(core::mem::align_of::<T>()) {
        return Err(Errno::EFAULT);
    }
    buffers.validate(ptr as usize, core::mem::size_of::<T>(), true)
}
pub(super) extern "C" fn kcore_ipc_listen(endpoint: u64) -> i32 {
    core_call(move || {
        status(transaction(
            Some(EndpointId::from_raw(endpoint)),
            true,
            |owner, task, state, _| state.listen(owner, task, EndpointId::from_raw(endpoint)),
        ))
    })
}
pub(super) extern "C" fn kcore_ipc_grant(endpoint: u64, consumer: u32) -> i32 {
    core_call(move || {
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
    core_call(move || {
        if let Err(e) = buffer(bytes as usize, len) {
            return e.code();
        }
        match transaction(
            Some(EndpointId::from_raw(endpoint)),
            false,
            |owner, task, state, buffers| {
                output(buffers, request)?;
                let mut local = [0; exchange::MESSAGE_MAX];
                buffers.read(bytes as usize, &mut local[..len])?;
                let (id, wake) =
                    state.submit(owner, task, EndpointId::from_raw(endpoint), &local[..len])?;
                buffers.put(request, id)?;
                Ok(wake)
            },
        ) {
            Ok(wake) => {
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
    core_call(move || {
        if let Err(e) = buffer(bytes as usize, capacity) {
            return e.code();
        }
        status(transaction(
            Some(EndpointId::from_raw(endpoint)),
            true,
            |_, task, state, buffers| {
                buffers.validate(bytes as usize, capacity, true)?;
                output(buffers, request)?;
                output(buffers, consumer)?;
                output(buffers, consumer_task)?;
                output(buffers, length)?;
                let mut local = [0; exchange::MESSAGE_MAX];
                let (id, caller, task, len) =
                    state.receive(task, EndpointId::from_raw(endpoint), &mut local[..capacity])?;
                buffers.write(bytes as usize, &local[..len])?;
                buffers.put(request, id)?;
                buffers.put(consumer, caller.raw())?;
                buffers.put(consumer_task, task.raw())?;
                buffers.put(length, len)
            },
        ))
    })
}
pub(super) extern "C" fn kcore_ipc_reply(request: u64, bytes: *const u8, len: usize) -> i32 {
    core_call(move || {
        if let Err(e) = buffer(bytes as usize, len) {
            return e.code();
        }
        match transaction(None, false, |_, task, state, buffers| {
            let mut local = [0; exchange::MESSAGE_MAX];
            buffers.read(bytes as usize, &mut local[..len])?;
            state.reply(task, request, &local[..len])
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
    core_call(move || {
        if let Err(e) = buffer(bytes as usize, capacity) {
            return e.code();
        }
        status(transaction(None, false, |_, task, state, buffers| {
            buffers.validate(bytes as usize, capacity, true)?;
            output(buffers, length)?;
            output(buffers, completion)?;
            let mut local = [0; exchange::MESSAGE_MAX];
            let (status, len) = state.collect(task, request, &mut local[..capacity])?;
            buffers.write(bytes as usize, &local[..len])?;
            buffers.put(length, len)?;
            buffers.put(completion, status)
        }))
    })
}
pub(super) extern "C" fn kcore_ipc_wait(endpoint: u64, request: u64) -> i32 {
    core_call(move || {
        match transaction(
            (request == 0).then_some(EndpointId::from_raw(endpoint)),
            true,
            |_, task, state, _| state.wait(task, EndpointId::from_raw(endpoint), request),
        ) {
            Ok(true) => status(sched::park_current()),
            Ok(false) => 0,
            Err(e) => e.code(),
        }
    })
}
pub(super) extern "C" fn kcore_ipc_cancel(request: u64) -> i32 {
    core_call(move || {
        match transaction(None, false, |_, task, state, _| state.cancel(task, request)) {
            Ok(wake) => {
                exchange::wake(wake);
                0
            }
            Err(e) => e.code(),
        }
    })
}
pub(super) extern "C" fn kcore_ipc_close(endpoint: u64) -> i32 {
    core_call(move || {
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
        endpoints.invalidate(id);
        let wakes = exchange::get().lock().close(id);
        drop(endpoints);
        drop(registry);
        exchange::wake(wakes.into_iter().flatten());
        0
    })
}
