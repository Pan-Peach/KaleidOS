//! 核心日志（启动/诊断/panic 的早期输出）。
//!
//! C-Lite 注入式设计（Oracle 裁决）：core 不知道 console 的传输方式 ——
//! 只暴露一个 `fn(&str)` 写入器槽位，由 bootstrap（或 host 测试）在启动时安装。
//! 绝不内联 SBI/arch 指令（`#[cfg(target_arch)]` 分支会随平台堆叠 → core 变成
//! console 后端切换台，违背"core 只收真相"）。
//!
//! - `install`：注册写入器（一次）
//! - `print`：无前缀输出 `args`
//! - `log`：`[tag] args\n`（Linux dmesg 风格，与 bootstrap 旧 console::log 一致）
//!
//! 未安装写入器时全部静默（包括 host test —— 测试里不注册就无输出；
//! 注册了才验证格式化）。
//! **注意**：panic 路径不走本模块 —— panic 可能发生在锁/堆损坏时，
//! 由 bootstrap 的静态紧急 console 直连输出（见 bootstrap console.rs）。

use core::fmt::{self, Write};
use spin::Mutex;

type WriteFn = fn(&str);
type ReadFn = fn() -> Option<u8>;

static WRITER: Mutex<Option<WriteFn>> = Mutex::new(None);
static READER: Mutex<Option<ReadFn>> = Mutex::new(None);

/// 安装日志写入器（仅允许一次；重复安装返回 Err）。
pub fn install(writer: WriteFn) -> Result<(), ()> {
    let mut slot = WRITER.lock();
    if slot.is_some() {
        return Err(());
    }
    *slot = Some(writer);
    Ok(())
}

/// 安装输入读取器（Monitor 用；仅一次）。
pub fn install_reader(reader: ReadFn) -> Result<(), ()> {
    let mut slot = READER.lock();
    if slot.is_some() {
        return Err(());
    }
    *slot = Some(reader);
    Ok(())
}

/// 无前缀输出（core 内部各模块的自描述日志用）。
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

// ---------------------------------------------------------------------------
// Monitor 输入：read_line（轮询 reader，直到 \n 或缓冲区满）
// ---------------------------------------------------------------------------

/// 读一行（最长 `max-1` 字节，留 NUL 终止）。回车(\n)结束；退格(0x08/0x7f)删字符。
/// 返回行字节数；空行（仅回车）返回 0。
/// 无 reader 安装时永不返回（monitor 无处输入，等待）。
pub fn read_line(buf: &mut [u8]) -> usize {
    // 先取 reader 函数（锁内只拷贝函数指针，锁外调用，避免 re-entrant）。
    let reader = *READER.lock();
    let reader = match reader {
        Some(r) => r,
        None => loop {
            // 未安装 reader：阻塞轮询（无输入来源，只能等）
            core::hint::spin_loop();
        },
    };

    let mut n = 0;
    loop {
        let Some(ch) = reader() else {
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

/// 打印任意字节序列（不经格式化，monitor 回显/原始输出用）。
pub fn print_bytes(bytes: &[u8]) {
    let mut sink = Sink;
    let _ = sink.write_str(core::str::from_utf8(bytes).unwrap_or("(?non-utf8)"));
}

// 转发到注册的写入器（无写入器则静默）。
struct Sink;

impl Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(w) = *WRITER.lock() {
            w(s);
        }
        Ok(())
    }
}
