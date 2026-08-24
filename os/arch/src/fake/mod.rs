use crate::{Arch, ResetType};

// 本 crate 整体 no_std；fake 仅在 host 编译（cfg 非 riscv64），显式引入 std 供 console 直通。
extern crate std;
use std::io::Write;

/// Host 实现：console 直通 std stdout/stdin。
///
/// - `console_write_byte`：逐字节写 stdout 并 flush——串口语义（无缓冲、保序），
///   让 `printk!`/`log!` 在 host 测试里真实可见（cargo test 会捕获到测试输出）。
/// - `console_getc`：恒返回 `None`。`core::print::read_line()` 会忙等轮询，
///   host 测试中调用它会挂死——需要交互输入时走 QEMU 层，不在 fake 里读 stdin。
pub struct Fake;

impl Arch for Fake {
    fn console_write_byte(byte: u8) {
        let mut out = std::io::stdout();
        let _ = out.write_all(&[byte]);
        let _ = out.flush();
    }

    fn console_getc() -> Option<u8> {
        None
    }

    fn system_reset(_reset_type: ResetType) -> ! {
        loop {
            core::hint::spin_loop();
        }
    }
}
