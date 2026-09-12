#![no_std]

// 只为组件私有 panic adapter 引入 SDK（不引用其符号，GC 后不进镜像）。
use kcomp_sdk as _;

// 刻意保留裸 extern 调用：host 测试用它验证「入口处 CALL 重定位到
// kcore_console_write_byte」的精确指令布局（loader.rs `relocation_writes_...`）。
unsafe extern "C" {
    #[link_name = "kcore_console_write_byte"]
    fn console_write_byte(byte: u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn kcomp_init() -> i32 {
    unsafe { console_write_byte(b'!') };
    0
}
