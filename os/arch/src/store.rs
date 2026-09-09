//! ComponentStore —— 组件仓库的传输层接口。
//!
//! 与 console backend 同层：ComponentStore 是 backend 接口，由具体 boot/profile
//! 实现（fake / RISC-V 各自给一个），Core 不直接依赖 QEMU 细节。
//! 未来 embedded init.kpkg / Persistent Store 也实现同一个 trait。

use alloc::vec::Vec;
extern crate alloc;

/// 仓库条目：名字 + 内容长度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreEntry {
    pub name: Vec<u8>,
    pub len: usize,
}

/// 仓库操作错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    NotFound,
    TooSmall,
    NotSupported,
}

/// 只读命名仓库 backend（实现由人类完成）。
pub trait ComponentStore: Sync {
    fn list(&self) -> Result<Vec<StoreEntry>, StoreError>;
    fn read(&self, name: &[u8], buf: &mut [u8]) -> Result<(), StoreError>;
}
