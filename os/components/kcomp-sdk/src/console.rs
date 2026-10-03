//! 当前诊断 console 的窄前端；输入暂经 Core，未来可绑定 Console service。

use crate::{Errno, Result, abi};

pub struct Console;

impl Console {
    pub fn write(bytes: &[u8]) {
        for &byte in bytes {
            crate::console_write_byte(byte);
        }
    }

    /// 空输入做一次有界 idle 等待后返回 None；任务可 yield 后继续轮询。
    pub fn read_byte() -> Result<Option<u8>> {
        // SAFETY: no borrowed pointers; Core owns the console backend.
        match unsafe { abi::kcore_console_read_byte() } {
            n @ 0..=255 => Ok(Some(n as u8)),
            n if n == Errno::EAGAIN.code() => Ok(None),
            n => Err(Errno::from_code(n)),
        }
    }
}

impl core::fmt::Write for Console {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        Self::write(value.as_bytes());
        Ok(())
    }
}
