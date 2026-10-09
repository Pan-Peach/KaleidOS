//! Actual generated clients/dispatchers and SDK envelope; only Core transport is fake.
use kcomp_sdk::generated::{block_wire, echo_wire};
pub use kcomp_sdk::{Errno, Result, endpoint, ipc};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
mod methods {
    include!("methods_wire.rs");
}

static REQUEST: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static CALLS: AtomicUsize = AtomicUsize::new(0);
struct Handler;
impl block_wire::Provider for Handler {
    fn capacity_sectors(&self) -> u64 {
        CALLS.fetch_add(1, Ordering::Relaxed);
        0x0102030405060708
    }
    fn read(&self, lba: u64, output: &mut [u8]) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        if lba == 13 {
            return Err(Errno::EIO);
        }
        output.fill(lba as u8);
        Ok(())
    }
    fn write(&self, _: u64, _: &[u8]) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
impl echo_wire::Provider for Handler {
    fn echo(&self, input: &[u8], output: &mut [u8]) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        output.copy_from_slice(input);
        Ok(())
    }
}
impl methods::Provider for Handler {
    fn numbers(
        &self,
        a_u8: u8,
        a_u16: u16,
        a_u32: u32,
        a_u64: u64,
        a_i8: i8,
        a_i16: i16,
        a_i32: i32,
        a_i64: i64,
    ) -> Result<methods::NumbersReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(methods::NumbersReply {
            b_u8: a_u8,
            b_u16: a_u16,
            b_u32: a_u32,
            b_u64: a_u64,
            b_i8: a_i8,
            b_i16: a_i16,
            b_i32: a_i32,
            b_i64: a_i64,
        })
    }
    fn flush(&self) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| u8::from_str_radix(std::str::from_utf8(v).unwrap(), 16).unwrap())
        .collect()
}
fn dispatch(bytes: &[u8], output: &mut [u8]) -> i32 {
    let request = match ipc::service::Request::decode(bytes) {
        Ok(r) => r,
        Err(e) => return e.code(),
    };
    match std::env::args().nth(2).unwrap().as_str() {
        "block" => block_wire::dispatch(&Handler, &request, output),
        "echo" => echo_wire::dispatch(&Handler, &request, output),
        _ => methods::dispatch(&Handler, &request, output),
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn kcore_ipc_submit(
    endpoint: u64,
    bytes: *const u8,
    len: usize,
    id: *mut u64,
) -> i32 {
    assert_eq!(endpoint, 7);
    let bytes = unsafe { std::slice::from_raw_parts(bytes, len) };
    println!("request={}", hex(bytes));
    if std::env::args().nth(5).as_deref() == Some("transport") {
        return Errno::EACCES.code();
    }
    *REQUEST.lock().unwrap() = bytes.to_vec();
    unsafe {
        *id = 1;
    }
    0
}
#[unsafe(no_mangle)]
unsafe extern "C" fn kcore_ipc_collect(
    id: u64,
    bytes: *mut u8,
    capacity: usize,
    len: *mut usize,
    completion: *mut i32,
) -> i32 {
    assert_eq!(id, 1);
    let request = REQUEST.lock().unwrap();
    let n = u32::from_le_bytes(request[4..8].try_into().unwrap()) as usize;
    assert!(capacity >= n + 4);
    let output = unsafe { std::slice::from_raw_parts_mut(bytes, n + 4) };
    output.fill(0);
    let status = dispatch(&request, &mut output[4..]);
    output[..4].copy_from_slice(&status.to_le_bytes());
    println!("reply={}", hex(output));
    unsafe {
        *len = n + 4;
        *completion = 0;
    }
    0
}
#[unsafe(no_mangle)]
extern "C" fn kcore_ipc_wait(_: u64, _: u64) -> i32 {
    panic!("unexpected wait")
}
#[unsafe(no_mangle)]
extern "C" fn kcore_ipc_cancel(_: u64) -> i32 {
    panic!("unexpected cancel")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args[1] == "dispatch" {
        let bytes = unhex(&args[3]);
        let mut output = vec![0; args[4].parse().unwrap()];
        let status = dispatch(&bytes, &mut output);
        println!(
            "status={status} calls={} output={}",
            CALLS.load(Ordering::Relaxed),
            hex(&output)
        );
        return;
    }
    let result = match args[2].as_str() {
        "echo" => {
            let input = unhex(&args[3]);
            let mut output = vec![0; args[4].parse().unwrap()];
            echo_wire::echo(7, &input, &mut output)
        }
        "block" => match args[3].as_str() {
            "capacity" => {
                block_wire::capacity_sectors(7).map(|v| assert_eq!(v, 0x0102030405060708))
            }
            "write" => block_wire::write(7, 7, &vec![0x81; args[4].parse().unwrap()]),
            _ => block_wire::read(
                7,
                args[3].parse().unwrap(),
                &mut vec![0; args[4].parse().unwrap()],
            ),
        },
        "numbers" => methods::numbers(
            7,
            0xff,
            0xabcd,
            0x89abcdef,
            0xfedcba9876543210,
            i8::MIN,
            i16::MIN,
            i32::MIN,
            i64::MIN,
        )
        .map(|r| assert_eq!(r.b_i64, i64::MIN)),
        "flush" => methods::flush(7),
        _ => panic!("unknown operation"),
    };
    match result {
        Ok(()) => println!(
            "transport=0 method=0 calls={}",
            CALLS.load(Ordering::Relaxed)
        ),
        Err(endpoint::InvokeError::Method(e)) => println!(
            "transport=0 method={} calls={}",
            e.code(),
            CALLS.load(Ordering::Relaxed)
        ),
        Err(endpoint::InvokeError::Transport(e)) => println!(
            "transport={} calls={}",
            e.code(),
            CALLS.load(Ordering::Relaxed)
        ),
        Err(e) => panic!("unexpected {e:?}"),
    }
}
