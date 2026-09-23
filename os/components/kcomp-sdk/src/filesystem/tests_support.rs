//! `filesystem` 契约的测试替身（`#[cfg(test)]`）：纯 Rust 实现
//! [`FileSystemProvider`]。
//!
//! 独立于 `crate::tests` 的 mock：那份服务 Direct（function table）路径，这份服务
//! Gate（dispatch / typed 前端）路径，两者互不牵连。

use core::ffi::CStr;

use crate::errno::{Errno, Result};
use crate::filesystem::FileSystemProvider;
use crate::generated::filesystem::KCOMP_FILESYSTEM_OPEN_READ;

/// mock `open` 返回的固定 handle（带高位，能暴露 LE 编码错位）。
pub(crate) const MOCK_HANDLE: u64 = 0x0102_0304_0506_0708;
/// mock `read` 的回填内容。
pub(crate) const MOCK_CONTENT: &[u8] = b"HELLO FROM FS";

/// `None` → `Ok`（read 回填 [`MOCK_CONTENT`]），`Some(e)` → `Err(e)`。
pub(crate) struct FileSystemMock {
    error: Option<Errno>,
}

impl FileSystemMock {
    pub(crate) const fn new(error: Option<Errno>) -> Self {
        Self { error }
    }
}

impl FileSystemProvider for FileSystemMock {
    fn mount(&self) -> Result<()> {
        self.error.map_or(Ok(()), Err)
    }

    fn unmount(&self) -> Result<()> {
        self.error.map_or(Ok(()), Err)
    }

    fn open(&self, path: &CStr, flags: u32) -> Result<u64> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if flags != KCOMP_FILESYSTEM_OPEN_READ {
            return Err(Errno::EROFS);
        }
        if path.to_bytes().is_empty() {
            return Err(Errno::ENOENT);
        }
        Ok(MOCK_HANDLE)
    }

    fn close(&self, handle: u64) -> Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if handle != MOCK_HANDLE {
            return Err(Errno::EBADF);
        }
        Ok(())
    }

    fn read(&self, handle: u64, buf: &mut [u8]) -> Result<usize> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if handle != MOCK_HANDLE {
            return Err(Errno::EBADF);
        }
        let len = buf.len().min(MOCK_CONTENT.len());
        buf[..len].copy_from_slice(&MOCK_CONTENT[..len]);
        Ok(len)
    }
}

/// 一被调用就 panic：证明适配器在 provider 之前挡下非法帧。
pub(crate) struct NeverCalled;

impl FileSystemProvider for NeverCalled {
    fn mount(&self) -> Result<()> {
        panic!("mount must not be called for a malformed frame")
    }

    fn unmount(&self) -> Result<()> {
        panic!("unmount must not be called for a malformed frame")
    }

    fn open(&self, _path: &CStr, _flags: u32) -> Result<u64> {
        panic!("open must not be called for a malformed frame")
    }

    fn close(&self, _handle: u64) -> Result<()> {
        panic!("close must not be called for a malformed frame")
    }

    fn read(&self, _handle: u64, _buf: &mut [u8]) -> Result<usize> {
        panic!("read must not be called for a malformed frame")
    }
}
