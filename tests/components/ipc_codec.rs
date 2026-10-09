//! Host harness for the production Rust envelope, with only transport replaced.
#![allow(dead_code)]
#[path = "../../os/components/kcomp-sdk/src/generated/abi.rs"]
pub mod abi;
#[path = "../../os/components/kcomp-sdk/src/generated/errno.rs"]
mod errno;
pub use errno::Errno;
pub type Result<T> = std::result::Result<T, Errno>;
#[path = "../../os/components/kcomp-sdk/src/ipc/service.rs"]
mod service;

mod ipc {
    use super::*;
    pub const MESSAGE_MAX: usize = abi::KCORE_IPC_MESSAGE_MAX as usize;
    pub fn call(endpoint: u64, request: &[u8], output: &mut [u8]) -> Result<usize> {
        assert_eq!(endpoint, 7);
        println!("request={}", hex(request));
        let args: Vec<String> = std::env::args().collect();
        if args[6] == "transport" {
            return Err(Errno::EIO);
        }
        let status: i32 = args[6].parse().unwrap();
        let len = u32::from_le_bytes(request[4..8].try_into().unwrap()) as usize + 4;
        output[..4].copy_from_slice(&status.to_le_bytes());
        output[4..len].fill(0xa5);
        Ok(len)
    }
    pub fn reply(_: u64, _: &[u8]) -> Result<()> {
        unreachable!("client codec harness has no server transport")
    }
}
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args[1] == "decode" {
        let bytes = unhex(&args[2]);
        match service::Request::decode(&bytes) {
            Ok(r) => println!(
                "method={} output={} args={} input={}",
                r.method,
                r.output,
                hex(r.args),
                hex(r.input)
            ),
            Err(e) => println!("error={}", e.code()),
        }
    } else {
        let input_args = unhex(&args[3]);
        let input = unhex(&args[4]);
        let mut output = vec![0; args[5].parse().unwrap()];
        match service::invoke(
            7,
            args[2].parse().unwrap(),
            &input_args,
            &input,
            &mut output,
        ) {
            Ok(status) => println!("transport=0 method={status} output={}", hex(&output)),
            Err(e) => println!("transport={}", e.code()),
        }
    }
}
