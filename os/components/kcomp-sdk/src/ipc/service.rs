//! Small SDK-owned service envelope. The Core sees only opaque owned bytes.
use crate::{Errno, Result, ipc};
pub const REQUEST_HEADER: usize = crate::abi::KCOMP_REQUEST_HEADER_LEN as usize;
pub const REPLY_HEADER: usize = crate::abi::KCOMP_REPLY_HEADER_LEN as usize;
pub struct Request<'a> {
    pub method: u32,
    pub args: &'a [u8],
    pub input: &'a [u8],
    pub output: usize,
}
impl<'a> Request<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() < REQUEST_HEADER {
            return Err(Errno::EINVAL);
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let output = word(4) as usize;
        let args = word(8) as usize;
        let input = word(12) as usize;
        let end_args = REQUEST_HEADER.checked_add(args).ok_or(Errno::EINVAL)?;
        if output > ipc::MESSAGE_MAX - REPLY_HEADER
            || end_args.checked_add(input) != Some(bytes.len())
        {
            return Err(Errno::EINVAL);
        }
        Ok(Self {
            method: word(0),
            args: &bytes[REQUEST_HEADER..end_args],
            input: &bytes[end_args..],
            output,
        })
    }
}
/// Input and reply are owned on this Task's stack; no borrow into a Core queue.
pub fn invoke(
    endpoint: u64,
    method: u32,
    args: &[u8],
    input: &[u8],
    output: &mut [u8],
) -> Result<i32> {
    let total = REQUEST_HEADER
        .checked_add(args.len())
        .and_then(|len| len.checked_add(input.len()))
        .ok_or(Errno::EMSGSIZE)?;
    if total > ipc::MESSAGE_MAX || output.len() > ipc::MESSAGE_MAX - REPLY_HEADER {
        return Err(Errno::EMSGSIZE);
    }
    let mut request = [0; ipc::MESSAGE_MAX];
    for (index, value) in [
        method,
        output.len() as u32,
        args.len() as u32,
        input.len() as u32,
    ]
    .into_iter()
    .enumerate()
    {
        request[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    request[REQUEST_HEADER..REQUEST_HEADER + args.len()].copy_from_slice(args);
    request[REQUEST_HEADER + args.len()..total].copy_from_slice(input);
    let mut reply = [0; ipc::MESSAGE_MAX];
    let len = ipc::call(endpoint, &request[..total], &mut reply)?;
    if len != output.len() + REPLY_HEADER {
        return Err(Errno::EPROTO);
    }
    let status = i32::from_le_bytes(reply[..REPLY_HEADER].try_into().unwrap());
    if status > 0 {
        return Err(Errno::EPROTO);
    }
    output.copy_from_slice(&reply[REPLY_HEADER..len]);
    Ok(status)
}
pub fn reply(receipt: u64, status: i32, output: &mut [u8]) -> Result<()> {
    if output.len() < REPLY_HEADER || output.len() > ipc::MESSAGE_MAX || status > 0 {
        return Err(Errno::EINVAL);
    }
    output[..REPLY_HEADER].copy_from_slice(&status.to_le_bytes());
    ipc::reply(receipt, output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn envelope_rejects_truncation_overflow_and_oversized_output() {
        assert!(matches!(
            Request::decode(&[0; REQUEST_HEADER - 1]),
            Err(Errno::EINVAL)
        ));
        let mut bytes = [0; REQUEST_HEADER + 3];
        bytes[..4].copy_from_slice(&17u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
        bytes[16..].copy_from_slice(&[1, 2, 3]);
        let decoded = Request::decode(&bytes).unwrap();
        assert_eq!(decoded.method, 17);
        assert_eq!(decoded.args, [1]);
        assert_eq!(decoded.input, [2, 3]);
        bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(Request::decode(&bytes), Err(Errno::EINVAL)));
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&(ipc::MESSAGE_MAX as u32).to_le_bytes());
        assert!(matches!(Request::decode(&bytes), Err(Errno::EINVAL)));
    }
}
