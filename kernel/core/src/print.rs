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

static WRITER: Mutex<Option<WriteFn>> = Mutex::new(None);

/// 安装日志写入器（仅允许一次；重复安装返回 Err）。
pub fn install(writer: WriteFn) -> Result<(), ()> {
    let mut slot = WRITER.lock();
    if slot.is_some() {
        return Err(());
    }
    *slot = Some(writer);
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

/// 转发到注册的写入器（无写入器则静默）。
struct Sink;

impl Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(w) = *WRITER.lock() {
            w(s);
        }
        Ok(())
    }
}
