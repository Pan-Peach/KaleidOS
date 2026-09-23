//! `bind`：contract / abi 原样交给 Core、回复自洽性、传输失败分类。

use super::*;
use crate::endpoint::Contract;

/// `Endpoint::bind` 把 **exact contract + abi** 交给 Core（SDK 不自行比较、
/// 不自行发现）：`kcore_endpoint_bind` 记录的参数必须与契约身份逐位一致。
#[test]
fn bind_passes_the_contract_identity_to_core() {
    let _guard = test_support::lock();
    test_support::reset_script();
    let _ = direct_binding();

    let bind = test_support::last_bind().expect("stub recorded the bind");
    assert_eq!(bind.endpoint, 7);
    assert_eq!(bind.contract, <FileSystem as Contract>::ID);
    assert_eq!(bind.abi, <FileSystem as Contract>::ABI);
}

/// bind 的回复必须自洽：Direct 却没带 function table、未知机制编码 →
/// `InvalidReply`（**绝不**降级或猜测）。
#[test]
fn bind_rejects_null_api_and_unknown_mechanism() {
    let _guard = test_support::lock();

    test_support::reset_script();
    test_support::script_bind(abi::KCORE_ENDPOINT_MECHANISM_DIRECT, 0, 0);
    assert_eq!(endpoint().bind().err(), Some(InvokeError::InvalidReply));

    test_support::reset_script();
    test_support::script_bind(0xDEAD, 0, 0);
    assert_eq!(endpoint().bind().err(), Some(InvokeError::InvalidReply));
}

/// bind 的传输失败（endpoint 已死 / contract 不符等）→ `Transport`。
#[test]
fn bind_transport_failure_maps_to_transport() {
    let _guard = test_support::lock();

    test_support::reset_script();
    test_support::script_bind_error(Errno::ENOENT.code());
    assert_eq!(
        endpoint().bind().err(),
        Some(InvokeError::Transport(Errno::ENOENT))
    );
}
