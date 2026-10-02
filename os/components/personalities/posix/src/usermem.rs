//! 用户地址是请求数据，不是可解引用的 Core / KernelNative 指针。
//! TODO: 地址空间身份、范围校验、copy fault 与并发 unmap 的真实机制。

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserAddress(pub u64);

pub struct UserRange {
    pub start: UserAddress,
    pub len: u64,
}

pub struct UserMemory;

impl UserMemory {
    /// TODO: 经调用线程实际的 AS 验证 / 拷贝；不能 slice::from_raw_parts(user_va)。
    pub fn copy_in(&self, _range: &UserRange, _destination: &mut [u8]) -> Result<usize> {
        Err(Error::Unsupported)
    }

    pub fn copy_out(&self, _range: &UserRange, _source: &[u8]) -> Result<usize> {
        Err(Error::Unsupported)
    }
}
