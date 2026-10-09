use super::*;
use crate::generated::filesystem::{
    KCOMP_FILESYSTEM_METHOD_LOOKUP, KCOMP_FILESYSTEM_METHOD_NODE_INFO, KCOMP_FILESYSTEM_METHOD_ROOT,
};

#[test]
fn direct_node_calls_use_the_table() {
    let _guard = test_support::lock();
    test_support::reset_script();
    let binding = direct_binding();
    let root = binding.root().unwrap();
    assert_eq!(root, 11);
    let node = binding.lookup(root, b"HELLO.TXT", 1).unwrap();
    assert_eq!(node, 12);
    assert_eq!(binding.node_info(node).unwrap(), 1);
    assert!(test_support::last_call().is_none());
}

#[test]
fn gate_node_calls_encode_parent_and_raw_name() {
    let _guard = test_support::lock();
    let binding = gate_binding();
    let root = 0x0102_0304_0506_0708u64;
    test_support::reset_script();
    test_support::script_call(0, 0);
    test_support::script_call_reply(&root.to_le_bytes());
    assert_eq!(binding.root().unwrap(), root);
    let call = test_support::last_call().unwrap();
    assert_eq!(call.method, KCOMP_FILESYSTEM_METHOD_ROOT);
    assert!(call.args.is_empty() && call.input.is_empty());
    assert_eq!(call.output_len, 8);

    test_support::reset_script();
    test_support::script_call_reply(&12u64.to_le_bytes());
    assert_eq!(binding.lookup(root, b"HELLO.TXT", 1).unwrap(), 12);
    let call = test_support::last_call().unwrap();
    assert_eq!(call.method, KCOMP_FILESYSTEM_METHOD_LOOKUP);
    assert_eq!(&call.args[..8], &root.to_le_bytes());
    assert_eq!(&call.args[8..], &1u32.to_le_bytes());
    assert_eq!(call.input, b"HELLO.TXT");
    assert_eq!(call.output_len, 8);

    test_support::reset_script();
    test_support::script_call_reply(&1u32.to_le_bytes());
    assert_eq!(binding.node_info(12).unwrap(), 1);
    let call = test_support::last_call().unwrap();
    assert_eq!(call.method, KCOMP_FILESYSTEM_METHOD_NODE_INFO);
    assert_eq!(call.args, 12u64.to_le_bytes());
    assert!(call.input.is_empty());
    assert_eq!(call.output_len, 4);
}

#[test]
fn node_errors_reject_zero_tokens_and_empty_names() {
    let _guard = test_support::lock();
    let binding = gate_binding();
    test_support::reset_script();
    test_support::script_call_reply(&0u64.to_le_bytes());
    assert_eq!(binding.root(), Err(InvokeError::InvalidReply));
    assert_eq!(binding.lookup(11, b"A", 1), Err(InvokeError::InvalidReply));
    test_support::reset_script();
    assert_eq!(
        binding.lookup(11, b"", 1),
        Err(InvokeError::Method(Errno::EINVAL))
    );
    assert!(test_support::last_call().is_none());
    test_support::script_call(0, Errno::ENOENT.code());
    assert_eq!(
        binding.lookup(11, b"A", 1),
        Err(InvokeError::Method(Errno::ENOENT))
    );
}
