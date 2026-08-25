//! ComponentStore —— 组件仓库的传输层接口。
//!
//! 与 console 同层：Core 定义"打印"语义依赖 ArchImpl::console_write_byte；
//! 这里同理：ComponentStore 是 backend 接口，由 arch 实现（fake / riscv64 各自给一个），
//! Core 通过 `ArchImpl::component_store()` 拿仓库，不直接依赖 QEMU 细节。
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
