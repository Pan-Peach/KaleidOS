use crate::input::{Event, Input};
use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::console::Console;
use kcomp_sdk::{Errno, management};
// Written only during create, before the sole session task is published.
// Two 32-bit words preserve full EndpointId on RV32 without requiring AtomicU64.
static FILESYSTEM: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
// The sole session task owns this instance's writable image. Keep bounded
// history off the one-granule task stack, especially on RV32; no heap needed.
static mut INPUT: Input = Input::new();
static mut DECODED: [u8; crate::parser::LINE_MAX] = [0; crate::parser::LINE_MAX];
static mut DEFAULT_ROOT: [u8; 256] = [0; 256];
static ROOT_LEN: AtomicU32 = AtomicU32::new(0);

pub fn default_root() -> &'static [u8] {
    // Set during create before the single session Task starts.
    unsafe { &(&*core::ptr::addr_of!(DEFAULT_ROOT))[..ROOT_LEN.load(Ordering::Acquire) as usize] }
}

pub fn filesystem() -> u64 {
    u64::from(FILESYSTEM[0].load(Ordering::Acquire))
        | (u64::from(FILESYSTEM[1].load(Ordering::Acquire)) << 32)
}

fn refresh(input: &Input) {
    let (visible, back) = input.view();
    Console::write(b"\r\x1b[2Kksh> ");
    Console::write(visible);
    if back != 0 {
        let _ = write!(Console, "\x1b[{back}D");
    }
}

extern "C" fn session(_arg: *mut ()) {
    Console::write(
        b"KaleidOS ksh\ntype 'help' for commands; Up/Down: history, Tab: completion\nksh> ",
    );
    // SAFETY: create publishes exactly one session task per component instance;
    // no other entry accesses INPUT. Each loaded instance has its own image.
    let input = unsafe { &mut *core::ptr::addr_of_mut!(INPUT) };
    // SAFETY: like INPUT, only the sole task uses this separate parsing buffer.
    let decoded = unsafe { &mut *core::ptr::addr_of_mut!(DECODED) };
    loop {
        match Console::read_byte() {
            Ok(Some(byte)) => match input.feed(byte) {
                Event::None => {}
                Event::Echo(byte) => Console::write(&[byte]),
                Event::Refresh => refresh(input),
                Event::Complete => {
                    let (names, count) = input.complete();
                    if count > 1 {
                        Console::write(b"\n");
                        for name in &names[..count] {
                            Console::write(name);
                            Console::write(b"  ");
                        }
                        Console::write(b"\n");
                    } else if count == 0 {
                        Console::write(b"\x07");
                    }
                    refresh(input);
                }
                Event::Clear => {
                    Console::write(b"\x1b[2J\x1b[H");
                    refresh(input);
                }
                Event::Submit => {
                    Console::write(b"\n");
                    input.remember();
                    if crate::shell::execute(input.line(), input, decoded) {
                        break;
                    }
                    Console::write(b"ksh> ");
                }
                Event::Overflow => {
                    let _ = write!(
                        Console,
                        "\ninput rejected: line too long (max {} bytes)\nksh> ",
                        crate::parser::LINE_MAX
                    );
                }
                Event::Invalid => Console::write(b"\ninput rejected: ASCII input only\nksh> "),
                Event::Cancel => Console::write(b"^C\nksh> "),
                Event::Exit => {
                    Console::write(b"\n");
                    crate::shell::execute(b"exit", input, decoded);
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
    } else if args.config_abi == 0x72BD_51C9_340F_A806
        && (9..=264).contains(&args.config_len)
        && !args.config.is_null()
    {
        let config =
            unsafe { core::slice::from_raw_parts(args.config.cast::<u8>(), args.config_len) };
        let root = &config[8..];
        if !root.starts_with(b"/") || root.contains(&0) {
            return Errno::EINVAL.code();
        }
        unsafe {
            (&mut *core::ptr::addr_of_mut!(DEFAULT_ROOT))[..root.len()].copy_from_slice(root)
        };
        ROOT_LEN.store(root.len() as u32, Ordering::Release);
        u64::from_le_bytes(config[..8].try_into().unwrap())
    } else {
        return Errno::EINVAL.code();
    };
    if endpoint != 0
        && let Err(error) = kcomp_sdk::endpoint::Endpoint::<kcomp_sdk::vfs::Vfs>::from_id(endpoint)
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
