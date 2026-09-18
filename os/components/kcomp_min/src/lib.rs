#![no_std]

// 只为组件私有 panic adapter 引入 SDK（不引用其符号，GC 后不进镜像）。
use kcomp_sdk as _;
use kcomp_sdk::abi::{KCOMP_ABI, KcompCreateArgs};

// 刻意保留裸 extern 调用：host 测试用它验证「入口处 CALL 重定位到
// kcore_console_write_byte」的精确指令布局（loader.rs `relocation_writes_...`）。
unsafe extern "C" {
    #[link_name = "kcore_console_write_byte"]
    fn console_write_byte(byte: u8);
}

// 本组件**刻意手工写出**完整生命周期入口（不经 SDK 宏）：它是"契约只认符号与
// 签名"的最小参照——SDK 宏（`kcomp_instance_create!` 的 `|args, out_state|`
// 形式）只是便利，不是契约要求。无状态：成功不写 out_state（Core 调用前已把
// 它初始化为 NULL）。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_instance_create(
    _args: *const KcompCreateArgs,
    _out_state: *mut *mut (),
) -> i32 {
    unsafe { console_write_byte(b'!') };
    0
}

// 无状态组件的 destroy：没有私有资源可清，显式 no-op。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_instance_destroy(_state: *mut ()) -> i32 {
    0
}

// 精确契约指纹（与 `kcomp_instance_create!` 宏发出的 `kcomp_abi` 同符号同值）。
#[unsafe(no_mangle)]
pub static kcomp_abi: u64 = KCOMP_ABI;
