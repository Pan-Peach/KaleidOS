//! `kcomp_services!` 宏展开的 host 测试：签名锚定 / port switch / 未知 port /
//! 空 state 不 UB。
//!
//! **端口 pattern 重复 = 编译错误**由宏内逐对 `const` 断言保证；正向用例只能
//! 编译通过来证明，反向用例（重复端口）会红，不适合放进本测试套件（没有
//! compile-fail harness，也不引新依赖）。

use crate::abi::KcompCallFrame;
use crate::errno::Errno;
use crate::frame::Call;
const TEST_READ: u32 = 1;
struct State;
fn dispatch(_state: &State, method: u32, call: Call<'_>) -> i32 {
    if method != TEST_READ {
        return Errno::ENOSYS.code();
    }
    call.output.fill(0xa5);
    0
}

const BLOCK_PORT: u32 = 3;
const OTHER_PORT: u32 = 4;

static DEVICE: State = State;

crate::kcomp_services! {
    state: State;
    BLOCK_PORT => dispatch,
    OTHER_PORT => dispatch,
}

/// port switch：选中的 port → 契约 adapter（provider 真被调用）；未知 port /
/// 未知 method → `-ENOSYS`（能力缺失档位，不是 panic）。
#[test]
fn port_switch_routes_to_the_contract_adapter() {
    let state = &DEVICE as *const State as *mut ();
    let args = 1u64.to_le_bytes();
    let mut output = [0u8; 512];
    let frame = KcompCallFrame {
        args: args.as_ptr(),
        args_len: args.len(),
        input: core::ptr::null(),
        input_len: 0,
        output: output.as_mut_ptr(),
        output_len: output.len(),
    };

    assert_eq!(
        kcomp_service_dispatch(state, BLOCK_PORT, TEST_READ, &frame),
        0
    );
    assert_eq!(output, [0xA5; 512]);

    assert_eq!(
        kcomp_service_dispatch(state, 99, TEST_READ, &frame),
        Errno::ENOSYS.code()
    );
    assert_eq!(
        kcomp_service_dispatch(state, BLOCK_PORT, 99, &frame),
        Errno::ENOSYS.code()
    );
}

/// `instance_state == NULL` → `-EINVAL`（不构造 `&State`，无 UB）。
#[test]
fn null_state_is_rejected_before_any_dereference() {
    assert_eq!(
        kcomp_service_dispatch(
            core::ptr::null_mut(),
            BLOCK_PORT,
            TEST_READ,
            core::ptr::null(),
        ),
        Errno::EINVAL.code()
    );
}
