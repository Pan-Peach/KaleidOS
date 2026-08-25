//! 组件仓库（嵌入式）：从内核镜像的 `.initpkg` section 读 cpio 归档。
//!
//! 分层：trait 在 arch（os/arch/src/store.rs）；本模块是 core 侧实现 +
//! 全局注册。字节来源由 boot 注入（链接脚本 `__initpkg_start/__initpkg_end`）：
//! `store::init(blob)` 调用一次即挂载。
//! newc 解析（parse_entries/list/read）由人类实现。

use alloc::vec::Vec;
use arch::{ComponentStore, StoreEntry, StoreError};
use spin::Once;

/// 从 newc 归档解析出的一个条目（借用自 blob，零拷贝）。
pub struct CpioEntry<'a> {
    pub name: &'a [u8],
    pub data: &'a [u8],
}

/// 解析 newc 归档：blob → 条目列表（纯内存逻辑，host-testable）。
pub fn parse_entries(_blob: &'static [u8]) -> Result<Vec<CpioEntry<'static>>, StoreError> {
    todo!("人类实现：110 字节头解析 + 4 字节对齐迭代 + TRAILER 判定")
}

/// 嵌入式仓库：持有 .initpkg 字节切片（无状态，blob 即一切）。
#[allow(dead_code)] // blob 在 list/read 实现后读取
pub struct EmbeddedStore {
    blob: &'static [u8],
}

impl EmbeddedStore {
    pub const fn new(blob: &'static [u8]) -> Self {
        Self { blob }
    }
}

impl ComponentStore for EmbeddedStore {
    fn list(&self) -> Result<Vec<StoreEntry>, StoreError> {
        todo!("人类实现：parse_entries(self.blob) → StoreEntry 列表")
    }

    fn read(&self, _name: &[u8], _buf: &mut [u8]) -> Result<(), StoreError> {
        todo!("人类实现：按 name 找条目 → 越界检查后拷贝")
    }
}

static STORE: Once<EmbeddedStore> = Once::new();

/// 挂载仓库（boot 调用一次；blob 来自链接脚本 .initpkg section）。
pub fn init(blob: &'static [u8]) {
    STORE.call_once(|| EmbeddedStore::new(blob));
}

/// 取当前仓库；未挂载返回 None。
pub fn get_component_store() -> Option<&'static dyn ComponentStore> {
    STORE.get().map(|s| s as &dyn ComponentStore)
}
