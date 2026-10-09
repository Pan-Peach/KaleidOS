//! Gate `read` 的回复解码：8 字节长度头、数据区、超出容量的回复。

use super::*;
use crate::generated::filesystem::KCOMP_FILESYSTEM_METHOD_READ;

/// Gate `read`：args = LE handle、output = 调用方的整个 frame 区（8 字节头 + 数据），
/// 客户端从回复头解出实际长度；数据复制到业务缓冲区。
#[test]
fn gate_read_decodes_the_length_header() {
    let _guard = test_support::lock();
    test_support::reset_script();
    test_support::script_call(0, 0);

    let handle = 0x0102_0304_0506_0708u64;
    let payload = b"GATE REPLY DATA";
    let mut reply = std::vec::Vec::from((payload.len() as u64).to_le_bytes());
    reply.extend_from_slice(payload);
    test_support::script_call_reply(&reply);

    let mut region = [0u8; 8 + 32];
    let actual = gate_binding().read(handle, &mut region).unwrap();
    assert_eq!(actual, payload.len());
    assert_eq!(&region[..actual], payload);

    let call = test_support::last_call().unwrap();
    assert_eq!(call.method, KCOMP_FILESYSTEM_METHOD_READ);
    assert_eq!(call.args, handle.to_le_bytes());
    assert!(call.input.is_empty());
    assert_eq!(call.output_len, region.len() + 8, "output_len 含 8 字节头");
}

/// 传输成功但回复头声称的长度超过数据容量 → `InvalidReply`（不猜测、不截断）。
#[test]
fn gate_read_reply_header_larger_than_capacity_is_invalid_reply() {
    let _guard = test_support::lock();
    test_support::reset_script();
    test_support::script_call(0, 0);
    test_support::script_call_reply(&(255u64).to_le_bytes());

    let mut region = [0u8; 8 + 32];
    assert_eq!(
        gate_binding().read(1, &mut region),
        Err(InvokeError::InvalidReply)
    );
}

#[test]
fn gate_read_is_bounded_and_never_changes_buffer_on_invalid_reply() {
    let _guard = test_support::lock();
    test_support::reset_script();
    test_support::script_call(0, 0);
    test_support::script_call_reply(&513u64.to_le_bytes());
    let mut data = [0xa5; 1024];
    assert_eq!(
        gate_binding().read(1, &mut data),
        Err(InvokeError::InvalidReply)
    );
    assert_eq!(data, [0xa5; 1024]);
    assert_eq!(test_support::last_call().unwrap().output_len, 520);
}
