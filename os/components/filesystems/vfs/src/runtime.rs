//! Independent VFS Server Task; all filesystem objects remain in this image.
use crate::{
    Error, Result,
    local::{LocalEntry, LocalFs},
    namespace::{LookupContext, Namespace},
    provider::FileSystem,
    remote::RemoteFs,
    service::Service,
};
use alloc::{boxed::Box, vec::Vec};
use kcomp_sdk::{
    abi, ipc, management,
    vfs::{codec::*, *},
};
struct State {
    endpoints: Vec<u64>,
    control: u32,
}
fn consumer_alive(consumer: u32, task: u32) -> bool {
    let status = unsafe { abi::kcore_task_state(task) };
    if status < 0 || status == 4 {
        return false;
    }
    let mut name = [0; 256];
    for ordinal in 0..u32::MAX {
        match management::component_nth(ordinal, &mut name) {
            Ok(Some(row)) if row.id == consumer => return row.state == 2 || row.state == 3,
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    false
}
fn namespace(endpoints: &[u64]) -> Result<(Service, Vec<RemoteFs>)> {
    let local = LocalFs::new(&[
        LocalEntry {
            parent: 0,
            name: b"local",
            data: None,
        },
        LocalEntry {
            parent: 1,
            name: b"README.TXT",
            data: Some(b"KaleidOS local filesystem\n"),
        },
        LocalEntry {
            parent: 0,
            name: b"fat",
            data: None,
        },
        LocalEntry {
            parent: 0,
            name: b"second",
            data: None,
        },
    ])?;
    let mut namespace = Namespace::new(local.root()?)?;
    let mut remotes = Vec::new();
    let root = namespace.root();
    for (index, endpoint) in endpoints.iter().enumerate() {
        let remote = RemoteFs::connect(*endpoint)?;
        let at = namespace.resolve(
            &LookupContext {
                start: &root,
                root: &root,
                beneath: false,
                cross_mounts: true,
            },
            if index == 0 { b"fat" } else { b"second" },
        )?;
        namespace.attach(&at, remote.root()?)?;
        remotes.push(remote);
    }
    Ok((Service::new(namespace), remotes))
}
extern "C" fn server(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let mut service = namespace(&state.endpoints);
    let owner = management::current_component().unwrap();
    let endpoint = kcomp_sdk::endpoint::Endpoint::<Vfs>::lookup(owner, VFS_NAME)
        .unwrap()
        .id();
    ipc::listen(endpoint).unwrap();
    if state.control != 0 {
        ipc::grant(endpoint, state.control).unwrap();
    }
    let mut bytes = [0; ipc::MESSAGE_MAX];
    loop {
        let (receipt, consumer, task, len) = match ipc::receive(endpoint, &mut bytes) {
            Ok(message) => message,
            Err(Error::EAGAIN) => {
                if ipc::wait_receive(endpoint).is_err() {
                    break;
                }
                continue;
            }
            Err(_) => break,
        };
        let request = match ipc::service::Request::decode(&bytes[..len]) {
            Ok(request) => request,
            Err(error) => {
                let _ = ipc::reply(receipt, &error.code().to_le_bytes());
                continue;
            }
        };
        let mut reply = [0; ipc::MESSAGE_MAX];
        let output =
            &mut reply[ipc::service::REPLY_HEADER..ipc::service::REPLY_HEADER + request.output];
        let shutdown = request.method == KCOMP_VFS_METHOD_SHUTDOWN;
        let outcome = if shutdown {
            if consumer != state.control {
                Err(Error::EACCES)
            } else if !request.args.is_empty() || !request.input.is_empty() || output.len() != 8 {
                Err(Error::EINVAL)
            } else {
                Ok(None)
            }
        } else {
            match &mut service {
                Ok((service, _)) => {
                    service.reap(consumer_alive);
                    service.dispatch(consumer, task, &request, output)
                }
                Err(error) => Err(*error),
            }
        };
        let (status, undo) = match outcome {
            Ok(undo) => (0, undo),
            Err(error) => (error.code(), None),
        };
        // Always return a valid VFS status header, even for business errors.
        if output.len() >= 8 {
            put32(output, 0, 0);
            put32(output, 4, 0);
        }
        let committed = ipc::service::reply(
            receipt,
            status,
            &mut reply[..ipc::service::REPLY_HEADER + request.output],
        );
        if let Ok((service, remotes)) = &mut service {
            if committed.is_err()
                && let Some(undo) = undo
            {
                service.rollback(consumer, task, undo);
            }
            if shutdown && status == 0 {
                service.reap(|_, _| false);
            }
            for remote in remotes {
                let _ = remote.drain();
            }
        }
        if shutdown && status == 0 {
            let _ = ipc::close(endpoint);
            break;
        }
    }
    if let Ok((service, remotes)) = &mut service {
        service.reap(|_, _| false);
        for remote in remotes {
            let _ = remote.drain();
        }
    }
    management::exit_task();
}
kcomp_sdk::kcomp_instance_create!(|args, out_state| {
    let Some(args) = (unsafe { args.as_ref() }) else {
        return Error::EINVAL.code();
    };
    let (control, endpoints) = if args.config_abi == 0 && args.config_len == 0 {
        (0, Vec::new())
    } else {
        if args.config_abi != KCOMP_VFS_CREATE_CONFIG_ABI
            || args.config.is_null()
            || args.config_len < 8
        {
            return Error::EINVAL.code();
        }
        let bytes =
            unsafe { core::slice::from_raw_parts(args.config.cast::<u8>(), args.config_len) };
        let count = u32_at(bytes, 4) as usize;
        if count > 2 || bytes.len() != 8 + count * 8 {
            return Error::EINVAL.code();
        }
        let mut endpoints = Vec::new();
        for index in 0..count {
            let id = u64_at(bytes, 8 + index * 8);
            if id == 0 || endpoints.contains(&id) {
                return Error::EINVAL.code();
            }
            endpoints.push(id);
        }
        (u32_at(bytes, 0), endpoints)
    };
    let state = Box::into_raw(Box::new(State { control, endpoints }));
    unsafe { out_state.write(state.cast()) };
    let mut task = 0;
    let rc = unsafe { abi::kcore_task_create(server, state.cast(), &mut task) };
    if rc != 0 {
        unsafe {
            drop(Box::from_raw(state));
            out_state.write(core::ptr::null_mut())
        };
        return rc;
    }
    let rc = unsafe {
        abi::kcore_endpoint_publish(
            VFS_NAME.as_ptr(),
            VFS_NAME.len(),
            KCOMP_VFS_CONTRACT,
            1,
            KCOMP_VFS_ABI,
            0,
            core::ptr::null(),
            core::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return rc;
    }
    unsafe { abi::kcore_task_start(task) }
});
kcomp_sdk::kcomp_instance_destroy!(|state| {
    // Core stop rejects a live Task. The server must have drained and exited.
    if !state.is_null() {
        unsafe {
            drop(Box::from_raw(state.cast::<State>()));
        }
    }
    0
});
