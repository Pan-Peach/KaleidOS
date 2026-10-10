//! Actual generated clients/dispatchers and SDK envelope; only Core transport is fake.
use kcomp_sdk::generated::{
    block_wire, echo_wire, probe_wire as probe, filesystem_wire as fs, posix_wire as posix, vfs_wire as vfs,
};
use kcomp_sdk::vfs::*;
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
fn token() -> VfsPath {
    VfsPath {
        mount: 1,
        entry: 2,
        fs: 3,
        node: 4,
    }
}
fn status() -> VfsReplyStatus {
    VfsReplyStatus {
        domain: 0,
        reserved: 0,
    }
}
impl vfs::Provider for Handler {
    fn root(&mut self) -> Result<vfs::RootReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(vfs::RootReply {
            reply_status: status(),
            token: token(),
        })
    }
    fn resolve(&mut self, options: VfsLookup, _: &[u8]) -> Result<vfs::ResolveReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        assert_eq!(options.start, token());
        Ok(vfs::ResolveReply {
            reply_status: status(),
            token: token(),
        })
    }
    fn node_info(&mut self, _: VfsPath) -> Result<vfs::NodeInfoReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(vfs::NodeInfoReply {
            reply_status: status(),
            info: VfsNodeInfo {
                kind: 1,
                valid: 0,
                link_count: 0,
                name_encoding: 0,
                case_rule: 0,
            },
        })
    }
    fn read_dir(&mut self, _: VfsPath, _: u64, _: &mut [u8]) -> Result<vfs::ReadDirReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Err(Errno::ENOTSUP)
    }
    fn open(&mut self, options: VfsOpenRequest, _: &[u8]) -> Result<vfs::OpenReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        assert_eq!(options.path, token());
        Ok(vfs::OpenReply {
            reply_status: status(),
            file: 42,
        })
    }
    fn retain(&mut self, _: u64) -> Result<VfsReplyStatus> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(status())
    }
    fn read(&mut self, _: u64, output: &mut [u8]) -> Result<vfs::ReadReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        let actual = output.len().min(3);
        output[..actual].copy_from_slice(&b"abc"[..actual]);
        Ok(vfs::ReadReply {
            reply_status: status(),
            actual: actual as u64,
        })
    }
    fn read_at(&mut self, _: u64, _: u64, output: &mut [u8]) -> Result<vfs::ReadAtReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        let actual = output.len().min(3);
        output[..actual].copy_from_slice(&b"abc"[..actual]);
        Ok(vfs::ReadAtReply {
            reply_status: status(),
            actual: actual as u64,
        })
    }
    fn set_position(&mut self, _: u64, _: u64) -> Result<VfsReplyStatus> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(status())
    }
    fn stream_info(&mut self, _: u64) -> Result<vfs::StreamInfoReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(vfs::StreamInfoReply {
            reply_status: status(),
            info: VfsStreamInfo {
                stream: VfsStream {
                    fs: 3,
                    node: 4,
                    stream: 1,
                },
                size: 23,
                allocated_size: 0,
                valid_data_length: 0,
                valid: 0,
                reserved: 0,
            },
        })
    }
    fn close(&mut self, _: u64) -> Result<VfsReplyStatus> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(status())
    }
    fn retain_path(&mut self, _: VfsPath) -> Result<VfsReplyStatus> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(status())
    }
    fn release_path(&mut self, _: VfsPath) -> Result<VfsReplyStatus> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(status())
    }
    fn shutdown(&mut self) -> Result<VfsReplyStatus> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(status())
    }
}
impl fs::Provider for Handler {
    fn mount(&self) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn unmount(&self) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn open(&self, _: u32, _: &[u8]) -> Result<u64> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(42)
    }
    fn close(&self, _: u64) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn read(&self, _: u64, _: &mut [u8]) -> Result<u64> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(0)
    }
    fn root(&self) -> Result<u64> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(42)
    }
    fn lookup(&self, _: u64, _: u32, _: &[u8]) -> Result<u64> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(42)
    }
    fn node_info(&self, _: u64) -> Result<u32> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(1)
    }
    fn node_details(&self, _: u64, output: &mut [u8]) -> Result<fs::NodeDetailsReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        output.fill(0);
        output[..9].copy_from_slice(b"HELLO.TXT");
        Ok(fs::NodeDetailsReply {
            kind: 1,
            name_length: 9,
            size: 23,
        })
    }
    fn open_node(&self, _: u64) -> Result<u64> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(42)
    }
    fn read_at(&self, _: u64, offset: u64, output: &mut [u8]) -> Result<u64> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        if offset == u64::MAX {
            return Err(Errno::EOVERFLOW);
        }
        let actual = output.len().min(3);
        output[..actual].copy_from_slice(&b"abc"[..actual]);
        Ok(actual as u64)
    }
    fn shutdown(&self) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
impl probe::Provider for Handler {
    fn result(&self) -> Result<probe::ResultReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(probe::ResultReply { outcome: 1, detail: 0x01020304 })
    }
    fn shutdown(&self) -> Result<()> { CALLS.fetch_add(1, Ordering::Relaxed); Ok(()) }
}
impl posix::Provider for Handler {
    fn status(&self) -> Result<posix::StatusReply> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(posix::StatusReply {
            exited: 1,
            wait_status: 1792,
            live: 0,
        })
    }
    fn shutdown(&self) -> Result<()> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
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
        "probe" => probe::dispatch(&Handler, &request, output),
        "posix" => posix::dispatch(&Handler, &request, output),
        "block" => block_wire::dispatch(&Handler, &request, output),
        "echo" => echo_wire::dispatch(&Handler, &request, output),
        "filesystem" => fs::dispatch(&Handler, &request, output),
        "vfs" => vfs::dispatch(&mut Handler, &request, output),
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
    let mut status = dispatch(&request, &mut output[4..]);
    if std::env::args().nth(5).as_deref() == Some("domain") {
        status = -4095;
        vfs::encode_vfs_reply_status(
            &VfsReplyStatus {
                domain: 5,
                reserved: 0,
            },
            &mut output[4..12],
        )
        .unwrap();
    }
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
    if args[1] == "codec" {
        let input = unhex(&args[3]);
        let mut output = vec![0; args[4].parse().unwrap()];
        let result = vfs::decode_vfs_lookup(&input)
            .and_then(|value| vfs::encode_vfs_lookup(&value, &mut output));
        println!(
            "status={} output={}",
            result.err().map_or(0, |e| e.code()),
            hex(&output)
        );
        return;
    }
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
    if args[2] == "vfs" {
        let result = match args[3].as_str() {
            "root" => vfs::root(7).map(|(status, reply)| {
                if status == -4095 {
                    assert_eq!(reply.reply_status.domain, 5);
                }
                status
            }),
            "resolve" => vfs::resolve(
                7,
                VfsLookup {
                    start: token(),
                    root: token(),
                    flags: 6,
                    max_symlinks: 0,
                    encoding: 1,
                    reserved: 0,
                },
                b"fat/HELLO.TXT",
            )
            .map(|(s, _)| s),
            "info" => vfs::node_info(7, token()).map(|(s, _)| s),
            "open" => vfs::open(
                7,
                VfsOpenRequest {
                    path: token(),
                    access: 1,
                    share: 1,
                    stream_kind: 0,
                    encoding: 0,
                },
                &[],
            )
            .map(|(s, _)| s),
            "close" => vfs::close(7, 42).map(|(s, _)| s),
            "stream" => vfs::stream_info(7, 42).map(|(s, _)| s),
            _ => vfs::read_at(7, 42, 7, &mut vec![0; args[4].parse().unwrap()]).map(|(s, _)| s),
        };
        match result {
            Ok(s) => println!(
                "transport=0 method={s} calls={}",
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
        return;
    }
    let result = match args[2].as_str() {
        "probe" => {
            if args[3] == "shutdown" { probe::shutdown(7) }
            else { probe::result(7).map(|reply| assert_eq!(reply.detail, 0x01020304)) }
        }
        "posix" => {
            if args[3] == "shutdown" {
                posix::shutdown(7)
            } else {
                posix::status(7).map(|reply| assert_eq!(reply.wait_status, 1792))
            }
        }
        "filesystem" => match args[3].as_str() {
            "root" => fs::root(7).map(|v| assert_eq!(v, 42)),
            "lookup" => fs::lookup(7, 42, 1, b"HELLO.TXT").map(|v| assert_eq!(v, 42)),
            "details" => fs::node_details(7, 42, &mut vec![0; args[4].parse().unwrap()])
                .map(|r| assert_eq!(r.size, 23)),
            "open" => fs::open_node(7, 42).map(|v| assert_eq!(v, 42)),
            "close" => fs::close(7, 42),
            _ => fs::read_at(7, 42, 7, &mut vec![0; args[4].parse().unwrap()]).map(|_| ()),
        },
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
