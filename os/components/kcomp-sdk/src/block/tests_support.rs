//! `block` 契约的测试替身（`#[cfg(test)]`）：纯 Rust 实现 [`BlockDeviceProvider`]。
//!
//! 独立于 `crate::tests` 的同名 mock：那份服务 Direct（function table）路径，
//! 这份服务 Gate（dispatch / typed 前端）路径，两者互不牵连。

use crate::block::BlockDeviceProvider;
use crate::errno::{Errno, Result};

/// `None` → `Ok`（read 回填 `0xA5`），`Some(e)` → `Err(e)`。
pub(crate) struct BlockMock {
    capacity: u64,
    error: Option<Errno>,
}

impl BlockMock {
    pub(crate) const fn new(capacity: u64, error: Option<Errno>) -> Self {
        Self { capacity, error }
    }
}

impl BlockDeviceProvider for BlockMock {
    fn capacity_sectors(&self) -> u64 {
        self.capacity
    }

    fn read(&self, _lba: u64, buf: &mut [u8]) -> Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        buf.fill(0xA5);
        Ok(())
    }

    fn write(&self, _lba: u64, _buf: &[u8]) -> Result<()> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(())
    }
}

/// 一被调用就 panic：证明适配器在 provider 之前挡下非法帧。
pub(crate) struct NeverCalled;

impl BlockDeviceProvider for NeverCalled {
    fn capacity_sectors(&self) -> u64 {
        panic!("capacity must not be called for a malformed frame")
    }

    fn read(&self, _lba: u64, _buf: &mut [u8]) -> Result<()> {
        panic!("read must not be called for a malformed frame")
    }

    fn write(&self, _lba: u64, _buf: &[u8]) -> Result<()> {
        panic!("write must not be called for a malformed frame")
    }
}
