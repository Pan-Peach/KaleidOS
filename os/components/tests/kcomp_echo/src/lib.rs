//! Independent real Server Task, copied IPC only. No Direct table/dispatcher.
#![no_std]
#[path = "../contract.rs"]
mod contract;
use contract::*;
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::{Errno, abi, ipc, management};
static CONSUMER: AtomicU32 = AtomicU32::new(0);
extern "C" fn server(_arg: *mut ()) {
    let owner = management::current_component().unwrap();
    let mut endpoint = 0;
    assert_eq!(
        unsafe {
            abi::kcore_endpoint_lookup(owner, NAME.as_ptr(), NAME.len(), CONTRACT, &mut endpoint)
        },
        0
    );
    ipc::listen(endpoint).unwrap();
    ipc::grant(endpoint, CONSUMER.load(Ordering::Relaxed)).unwrap();
    // Owner can propose a self-call; Core rejects the wait cycle, not grants.
    ipc::grant(endpoint, owner).unwrap();
    let mut bytes = [0; ipc::MESSAGE_MAX];
    loop {
        let (request, consumer, _task, len) = match ipc::receive(endpoint, &mut bytes) {
            Ok(message) => message,
            Err(Errno::EAGAIN) => {
                ipc::wait_receive(endpoint).unwrap();
                continue;
            }
            Err(Errno::ENOENT) => management::exit_task(),
            Err(error) => panic!("echo receive: {:?}", error),
        };
        assert_eq!(consumer, CONSUMER.load(Ordering::Relaxed));
        match &bytes[..len] {
            [STOP] => {
                ipc::reply(request, &[]).unwrap();
                ipc::close(endpoint).unwrap();
                management::exit_task();
            }
            [PANIC] => panic!("injected IPC server failure"),
            [EXIT] => management::exit_task(),
            [SELF_CALL] => {
                assert_eq!(ipc::submit(endpoint, &[]), Err(Errno::EDEADLK));
                ipc::reply(request, &[]).unwrap();
            }
            [FOREIGN_GRANT, target @ ..] if target.len() == 8 => {
                let target = u64::from_le_bytes(target.try_into().unwrap());
                assert_eq!(ipc::grant(target, owner), Err(Errno::EACCES));
                ipc::reply(request, &[]).unwrap();
            }
            bytes
                if bytes.len() >= ipc::service::REQUEST_HEADER
                    && bytes[..4] == SERVICE_ECHO.to_le_bytes() =>
            {
                let frame = ipc::service::Request::decode(bytes).unwrap();
                assert!(frame.args.is_empty());
                assert_eq!(frame.output, frame.input.len());
                let mut reply = [0; ipc::MESSAGE_MAX];
                let len = ipc::service::REPLY_HEADER + frame.output;
                reply[ipc::service::REPLY_HEADER..len].copy_from_slice(frame.input);
                ipc::service::reply(request, 0, &mut reply[..len]).unwrap();
            }
            _ => {
                let result = ipc::reply(request, &bytes[..len]);
                assert!(result == Ok(()) || result == Err(Errno::ECANCELED));
            }
        }
    }
}
kcomp_sdk::kcomp_instance_create!(|args, out_state| {
    if args.is_null() {
        return Errno::EFAULT.code();
    }
    let args = unsafe { &*args };
    if args.config_abi != CONFIG_ABI || args.config_len != 8 || args.config.is_null() {
        return Errno::EINVAL.code();
    }
    let bytes = unsafe { core::slice::from_raw_parts(args.config.cast::<u8>(), 8) };
    let consumer = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    let cpu = u32::from_le_bytes(bytes[4..].try_into().unwrap());
    CONSUMER.store(consumer, Ordering::Relaxed);
    unsafe { out_state.write(core::ptr::null_mut()) };
    let mut task = 0;
    let status = unsafe { abi::kcore_task_create(server, core::ptr::null_mut(), &mut task) };
    if status != 0 {
        return status;
    }
    let status = unsafe {
        abi::kcore_endpoint_publish(
            NAME.as_ptr(),
            NAME.len(),
            CONTRACT,
            1,
            ABI,
            0,
            core::ptr::null(),
            core::ptr::null_mut(),
        )
    };
    if status != 0 {
        return status;
    }
    unsafe { abi::kcore_task_start_on(task, cpu) }
});
kcomp_sdk::kcomp_instance_destroy!(|_state| { 0 });
