//! FakeStore：host 侧组件仓库（内存实现）—— 与 Fake backend 同层。

use crate::store::{ComponentStore, StoreEntry, StoreError};
extern crate alloc;
use alloc::vec::Vec;

/// 内存仓库：(name, bytes) 表。
pub struct FakeStore {
    blobs: Vec<(Vec<u8>, Vec<u8>)>,
}

impl FakeStore {
    pub fn new() -> Self {
        Self { blobs: Vec::new() }
    }

    /// 塞一个 (name, content) 条目（测试/开发用）。
    pub fn add(&mut self, name: &[u8], content: &[u8]) {
        self.blobs.push((name.to_vec(), content.to_vec()));
    }
}

impl Default for FakeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ComponentStore for FakeStore {
    fn list(&self) -> Result<Vec<StoreEntry>, StoreError> {
        Ok(self
            .blobs
            .iter()
            .map(|(name, content)| StoreEntry {
                name: name.clone(),
                len: content.len(),
            })
            .collect())
    }

    fn read(&self, name: &[u8], buf: &mut [u8]) -> Result<(), StoreError> {
        let (_, content) = self
            .blobs
            .iter()
            .find(|(n, _)| n.as_slice() == name)
            .ok_or(StoreError::NotFound)?;
        if buf.len() < content.len() {
            return Err(StoreError::TooSmall);
        }
        buf[..content.len()].copy_from_slice(content);
        Ok(())
    }
}
