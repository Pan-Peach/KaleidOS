//! 启动 console —— 最小原始写入器（无格式、无前缀）。
//!
//! 唯一职责：把字节送到 arch crate 的 Console backend。
//! 前缀/格式由 `kernel::print`（core 的注入式日志）统一处理。
//! panic 路径用 `DirectWriter` 直接调 `write_byte`（panic 时 print 的锁可能
//! 已损坏，且 panic 消息不经过任何格式化）。

use arch::{Console, ConsoleImpl};

/// Write one byte without going through the formatted printer.
#[inline]
pub fn write_byte(byte: u8) {
    ConsoleImpl::write_byte(byte);
}
