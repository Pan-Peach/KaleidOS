//! 日志：固定缓冲 + `kcore_log_line`（无 alloc、无锁；行尾由 Core 追加）。
//!
//! 组件日志只经 Core 导出 `kcore_log_line`；本模块不分配、不持锁，可在 panic
//! 路径使用（panic adapter 复用 [`LineBuffer`] 与 [`LOG_LINE_BYTES`]）。

use crate::abi;

/// 单行诊断上限（超长截断；panic 路径不需要无限长消息）。
pub(crate) const LOG_LINE_BYTES: usize = 256;

/// 把 `core::fmt` 写进固定栈缓冲。
pub(crate) struct LineBuffer<'a> {
    pub(crate) bytes: &'a mut [u8],
    pub(crate) length: usize,
}

impl core::fmt::Write for LineBuffer<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let source = value.as_bytes();
        let remaining = self.bytes.len() - self.length;
        let take = source.len().min(remaining);
        self.bytes[self.length..self.length + take].copy_from_slice(&source[..take]);
        self.length += take;
        Ok(())
    }
}

/// 输出一行：`core::fmt` 格式化到栈缓冲，再一次 `kcore_log_line`（Core 加
/// `[kcomp] ` 前缀与换行）。不分配、不持锁，可在 panic 路径使用。
pub fn log(args: core::fmt::Arguments<'_>) {
    let mut bytes = [0u8; LOG_LINE_BYTES];
    let mut writer = LineBuffer {
        bytes: &mut bytes,
        length: 0,
    };
    let _ = core::fmt::write(&mut writer, args);
    let length = writer.length;
    // SAFETY: (ptr, len) 指向本帧内已初始化的字节；Core 只读该区间。
    unsafe {
        abi::kcore_log_line(bytes.as_ptr(), length);
    }
}

/// 直写一字节（无缓冲；需要与日志行交错时用）。
pub fn console_write_byte(byte: u8) {
    // SAFETY: Core 保证该导出线程/中断安全地写 arch Console backend。
    unsafe {
        abi::kcore_console_write_byte(byte);
    }
}
