//! Copied Endpoint Request/Reply. IDs are identities, not owning capabilities.
//! Current execution support: real KernelNative Tasks only; no Gate/IRQ waits.
use crate::{Errno, Result, abi};
pub const MESSAGE_MAX: usize = abi::KCORE_IPC_MESSAGE_MAX as usize;

fn status(code: i32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Errno::from_code(code))
    }
}
pub fn listen(endpoint: u64) -> Result<()> {
    status(unsafe { abi::kcore_ipc_listen(endpoint) })
}
pub fn grant(endpoint: u64, consumer: u32) -> Result<()> {
    status(unsafe { abi::kcore_ipc_grant(endpoint, consumer) })
}
pub fn close(endpoint: u64) -> Result<()> {
    status(unsafe { abi::kcore_ipc_close(endpoint) })
}

/// One outstanding request per calling Task. Must collect its terminal result.
pub fn submit(endpoint: u64, input: &[u8]) -> Result<u64> {
    let mut id = 0;
    status(unsafe { abi::kcore_ipc_submit(endpoint, input.as_ptr(), input.len(), &mut id) })?;
    Ok(id)
}
pub fn receive(endpoint: u64, output: &mut [u8]) -> Result<(u64, u32, u32, usize)> {
    let mut id = 0;
    let mut consumer = 0;
    let mut consumer_task = 0;
    let mut length = 0;
    status(unsafe {
        abi::kcore_ipc_receive(
            endpoint,
            output.as_mut_ptr(),
            output.len(),
            &mut id,
            &mut consumer,
            &mut consumer_task,
            &mut length,
        )
    })?;
    Ok((id, consumer, consumer_task, length))
}
pub fn reply(receipt: u64, input: &[u8]) -> Result<()> {
    status(unsafe { abi::kcore_ipc_reply(receipt, input.as_ptr(), input.len()) })
}
pub fn collect(request: u64, output: &mut [u8]) -> Result<usize> {
    let mut len = 0;
    let mut completion = 0;
    status(unsafe {
        abi::kcore_ipc_collect(
            request,
            output.as_mut_ptr(),
            output.len(),
            &mut len,
            &mut completion,
        )
    })?;
    status(completion)?;
    Ok(len)
}
pub fn wait_request(request: u64) -> Result<()> {
    status(unsafe { abi::kcore_ipc_wait(0, request) })
}
pub fn wait_receive(endpoint: u64) -> Result<()> {
    status(unsafe { abi::kcore_ipc_wait(endpoint, 0) })
}
pub fn cancel(request: u64) -> Result<()> {
    status(unsafe { abi::kcore_ipc_cancel(request) })
}

pub fn call(endpoint: u64, input: &[u8], output: &mut [u8]) -> Result<usize> {
    let request = submit(endpoint, input)?;
    loop {
        match collect(request, output) {
            Err(Errno::EAGAIN) => {
                if let Err(error) = wait_request(request) {
                    let _ = cancel(request);
                    let _ = collect(request, &mut [0; MESSAGE_MAX]);
                    return Err(error);
                }
            }
            Err(Errno::EMSGSIZE) => {
                // Short output does not consume Core's result. Drain it here;
                // raw collect callers can instead retry with a larger buffer.
                let _ = collect(request, &mut [0; MESSAGE_MAX]);
                return Err(Errno::EMSGSIZE);
            }
            result => return result,
        }
    }
}

pub mod service;
