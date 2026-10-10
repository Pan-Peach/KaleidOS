//! Independent real Server Task, copied IPC only. No Direct table/dispatcher.
#![no_std]
#[path = "../contract.rs"]
mod contract;
use contract::*;
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::generated::echo_wire;
use kcomp_sdk::{Errno, abi, ipc, management};
struct Echo;
impl echo_wire::Provider for Echo {
    fn echo(&self, input: &[u8], output: &mut [u8]) -> kcomp_sdk::Result<()> {
        output.copy_from_slice(input);
        Ok(())
    }
}
static CONSUMER: AtomicU32 = AtomicU32::new(0);
static TARGET: [AtomicU32; 2] = [const { AtomicU32::new(0) }; 2];
fn target() -> u64 {
    (u64::from(TARGET[1].load(Ordering::Relaxed)) << 32)
        | u64::from(TARGET[0].load(Ordering::Relaxed))
}
static STARTED: AtomicU32 = AtomicU32::new(0);
extern "C" fn server(_arg: *mut ()) {
    STARTED.store(1, Ordering::Release);
    let owner = management::current_component().unwrap();
    let mut endpoint = 0;
    // start_on publishes work immediately; the remote Task can enter while
    // create still owns staged endpoints. Yield until their atomic commit.
    loop {
        let status = unsafe {
            abi::kcore_endpoint_lookup(owner, NAME.as_ptr(), NAME.len(), CONTRACT, &mut endpoint)
        };
        if status == 0 {
            break;
        }
        assert_eq!(status, Errno::ENOENT.code());
        management::yield_task().unwrap();
    }
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
        let _ = consumer; // Core Grant, rather than message bytes, authorizes callers.
        match &bytes[..len] {
            [FORWARD, payload @ ..] if payload.len() == 3 => {
                let mut output = [0; ipc::MESSAGE_MAX];
                let len = ipc::call(target(), payload, &mut output).unwrap();
                ipc::reply(request, &output[..len]).unwrap();
            }
            [INVALID_BUFFER] => {
                let mut probe = 0;
                assert_eq!(
                    unsafe { abi::kcore_ipc_submit(endpoint, core::ptr::null(), 1, &mut probe) },
                    Errno::EFAULT.code()
                );
                assert_eq!(
                    unsafe {
                        abi::kcore_ipc_submit(
                            endpoint,
                            bytes.as_ptr(),
                            1,
                            0x8020_0000usize as *mut u64,
                        )
                    },
                    Errno::EFAULT.code()
                );
                assert_eq!(
                    unsafe {
                        abi::kcore_task_create(
                            server,
                            core::ptr::null_mut(),
                            0x8020_0000usize as *mut u32,
                        )
                    },
                    Errno::EFAULT.code()
                );
                ipc::reply(request, &[]).unwrap();
            }
            [MEMORY_FAULT] => unsafe {
                // RV64 U must take a real load page fault on a supervisor page.
                // If it returns, the expected ENOTCONN assertion fails.
                let _ = core::ptr::read_volatile(0x8020_0000usize as *const u8);
                ipc::reply(request, &[]).unwrap();
            },
            [FAULT] => unsafe {
                core::arch::asm!("unimp");
            },
            [BUSY_ACK] => {
                ipc::reply(request, &[]).unwrap();
                loop {
                    core::hint::spin_loop();
                }
            }
            [BUSY] => loop {
                core::hint::spin_loop();
            },
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
                let mut reply = [0; ipc::MESSAGE_MAX];
                let len = ipc::service::REPLY_HEADER + frame.output;
                let status =
                    echo_wire::dispatch(&Echo, &frame, &mut reply[ipc::service::REPLY_HEADER..len]);
                ipc::service::reply(request, status, &mut reply[..len]).unwrap();
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
    if args.config_abi != CONFIG_ABI || !matches!(args.config_len, 8 | 16) || args.config.is_null()
    {
        return Errno::EINVAL.code();
    }
    let bytes = unsafe { core::slice::from_raw_parts(args.config.cast::<u8>(), args.config_len) };
    let consumer = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    let cpu = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if bytes.len() == 16 {
        let target = u64::from_le_bytes(bytes[8..].try_into().unwrap());
        TARGET[0].store(target as u32, Ordering::Relaxed);
        TARGET[1].store((target >> 32) as u32, Ordering::Relaxed);
    }
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
    let status = unsafe { abi::kcore_task_start_on(task, cpu) };
    if status != 0 {
        return status;
    }
    // Force the real SMP startup ordering in this fixture, instead of hoping
    // QEMU happens to schedule the remote Task before create returns.
    if unsafe { abi::kcore_cpu_current() } != cpu {
        let deadline = unsafe { abi::kcore_now() + abi::kcore_timebase_hz() * 10 };
        while STARTED.load(Ordering::Acquire) == 0 {
            if unsafe { abi::kcore_now() } >= deadline {
                return Errno::EIO.code();
            }
            core::hint::spin_loop();
        }
    }
    0
});
kcomp_sdk::kcomp_instance_destroy!(|_state| { 0 });
