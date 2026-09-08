//! 核心日志（启动/诊断/panic 的早期输出）。
//!
//! 分层：格式化在 core（`printk!`/`log!` 宏 → `print::print`），传输通过
//! arch crate 的 `Console` backend。Core 不依赖具体 SBI、UART 或 std 实现。
//!
//! - `printk!`：无前缀输出（裸打印）
//! - `log!(tag, ...)`：`[tag] ...\n`（Linux dmesg 风格）
//! - `print`：`printk!` 宏的底层（接受 fmt::Arguments）
//!
//! host 测试：Fake console 写 stdout，printk! 自然可见。
//! 裸机：Riscv64 console 走 SBI。
//!
//! **注意**：panic 路径不走本模块 —— panic 可能发生在锁/堆损坏时，
//! 由 bootstrap 的静态紧急 console 直连输出（见 bootstrap console.rs）。

use arch::{Console, ConsoleImpl};
use core::fmt::{self, Write};

/// 无前缀输出（core 内部各模块的自描述日志用）。
/// 传输到当前 `Console` backend（逐字节）。
pub fn print(args: fmt::Arguments<'_>) {
    let mut sink = Sink;
    let _ = sink.write_fmt(args);
}

/// 带前缀日志：`[tag] args\n`。
pub fn log(tag: &str, args: fmt::Arguments<'_>) {
    let mut sink = Sink;
    let _ = write!(sink, "[{}] ", tag);
    let _ = sink.write_fmt(args);
    let _ = sink.write_str("\n");
}

/// 打印任意字节序列（不经格式化，monitor 回显/原始输出用）。
pub fn print_bytes(bytes: &[u8]) {
    let mut sink = Sink;
    let _ = sink.write_str(core::str::from_utf8(bytes).unwrap_or("(?non-utf8)"));
}

// ---------------------------------------------------------------------------
// Monitor 输入：read_line（轮询 Console::getc，直到 \n 或缓冲区满）
// ---------------------------------------------------------------------------

/// 读一行（最长 `max-1` 字节，留 NUL 终止）。回车(\n)结束；退格(0x08/0x7f)删字符。
/// 返回行字节数；空行（仅回车）返回 0。
pub fn read_line(buf: &mut [u8]) -> usize {
    let mut n = 0;
    loop {
        let Some(ch) = ConsoleImpl::getc() else {
            continue;
        };
        match ch {
            b'\n' | b'\r' => return n,
            0x08 | 0x7f => {
                if n > 0 {
                    n -= 1;
                    print(format_args!("\u{8} \u{8}"));
                }
            }
            _ if n < buf.len() - 1 => {
                buf[n] = ch;
                n += 1;
                print(format_args!("{}", ch as char));
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Sink：把格式化结果逐个字节送给 Console backend
// ---------------------------------------------------------------------------

struct Sink;

impl Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            ConsoleImpl::write_byte(byte);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 宏：printk!（无前缀）/ log!（显式 tag）
// ---------------------------------------------------------------------------

/// 裸打印（无前缀、不自动加换行）：
/// printk!("core> ");
/// printk!("region {:#x}\n", addr);
#[macro_export]
macro_rules! printk {
    ($($arg:tt)*) => {
        $crate::print::print(core::format_args!($($arg)*))
    };
}

/// 带 tag 日志（Linux dmesg 风格）：
/// log!("memory", "init OK");
/// log!("memory", "region {:#x}", addr);
#[macro_export]
macro_rules! log {
    ($tag:expr, $($arg:tt)*) => {
        $crate::print::log($tag, core::format_args!($($arg)*))
    };
}
