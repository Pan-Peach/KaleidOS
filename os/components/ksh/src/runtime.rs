use crate::input::{Event, Input};
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::console::Console;
use kcomp_sdk::{Errno, management};
// Written only during create, before the sole session task is published.
// Two 32-bit words preserve full EndpointId on RV32 without requiring AtomicU64.
static FILESYSTEM: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

pub fn filesystem() -> u64 {
    u64::from(FILESYSTEM[0].load(Ordering::Acquire))
        | (u64::from(FILESYSTEM[1].load(Ordering::Acquire)) << 32)
}

extern "C" fn session(_arg: *mut ()) {
    Console::write(b"KaleidOS ksh\ntype 'help' for commands\nksh> ");
    let mut input = Input::new();
    loop {
        match Console::read_byte() {
            Ok(Some(byte)) => match input.feed(byte) {
                Event::None => {}
                Event::Echo(byte) => Console::write(&[byte]),
                Event::Erase => Console::write(b"\x08 \x08"),
                Event::Submit => {
                    Console::write(b"\n");
                    if crate::shell::execute(input.line()) {
                        break;
                    }
                    Console::write(b"ksh> ");
                }
                Event::Overflow => {
                    Console::write(b"\ninput rejected: line too long (max 128 bytes)\nksh> ")
                }
                Event::Cancel => Console::write(b"^C\nksh> "),
                Event::Exit => {
                    crate::shell::execute(b"exit");
                    break;
                }
            },
            Ok(None) => {
                if let Err(error) = management::yield_task() {
                    kcomp_sdk::klog!("ksh: yield failed: {error}");
                    break;
                }
            }
            Err(error) => {
                kcomp_sdk::klog!("ksh: console failed: {error}");
                break;
            }
        }
    }
    management::exit_task();
}

kcomp_sdk::kcomp_instance_create!(|args, _out_state| {
    // SAFETY: Core borrows this header and config until create returns.
    let Some(args) = (unsafe { args.as_ref() }) else {
        return Errno::EINVAL.code();
    };
    let endpoint = if args.config_abi == 0 && args.config_len == 0 {
        0
    } else if args.config_abi == 0x4B53_4846_5343_4647
        && args.config_len == 8
        && !args.config.is_null()
    {
        unsafe { core::ptr::read_unaligned(args.config.cast::<u64>()) }
    } else {
        return Errno::EINVAL.code();
    };
    if endpoint != 0
        && let Err(error) =
            kcomp_sdk::endpoint::Endpoint::<kcomp_sdk::filesystem::FileSystem>::from_id(endpoint)
    {
        return error.code();
    }
    FILESYSTEM[0].store(endpoint as u32, Ordering::Release);
    FILESYSTEM[1].store((endpoint >> 32) as u32, Ordering::Release);
    match management::start_task(session) {
        Ok(_) => 0,
        Err(error) => error.code(),
    }
});
kcomp_sdk::kcomp_instance_destroy!(|_state| { 0 });
