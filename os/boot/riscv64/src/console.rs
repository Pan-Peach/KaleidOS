//! 启动 console —— 最小原始写入器（无格式、无前缀）。
//!
//! 唯一职责：把字节送到 `arch_riscv64::sbi::console_write_byte`。
//! 前缀/格式由 `kernel::print`（core 的注入式日志）统一处理。
//! `puts_direct` 供 panic 路径使用（panic 时 print 的锁可能已损坏，
//! 直接调 SBI 输出；且 panic 消息不经过任何格式化）。

use arch::{Arch, ArchImpl};

/// 写原始字节串（无缓冲、无格式）。
pub fn write(s: &str) {
    for byte in s.as_bytes() {
        ArchImpl::console_write_byte(*byte);
    }
}

/// Write one byte without going through the formatted printer.
#[inline]
pub fn write_byte(byte: u8) {
    ArchImpl::console_write_byte(byte);
}

/// 读一个字符（无输入返回 None；Monitor 行输入用）。
pub fn getc() -> Option<u8> {
    ArchImpl::console_getc()
}

/// panic 紧急输出：同 write，但命名上强调"绕过 print 锁"。
#[inline]
pub fn puts_direct(s: &str) {
    write(s);
}
