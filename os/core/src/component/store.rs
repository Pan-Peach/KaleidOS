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

fn hex_u32(bytes: &[u8]) -> u32 {
    let mut val = 0u32;
    for &b in bytes {
        val <<= 4;
        val |= match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => panic!("invalid hex digit"),
        };
    }
    val
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// 解析 newc 归档：blob → 条目列表（纯内存逻辑，host-testable）。
pub fn parse_entries(blob: &'static [u8]) -> Result<Vec<CpioEntry<'static>>, StoreError> {
    let mut pos = 0;
    let mut entries = Vec::new();
    loop {
        if blob.len() - pos < 110 {
            return Err(StoreError::TooSmall);
        }
        let header = &blob[pos..pos + 110];
        if &header[..6] != b"070701" {
            return Err(StoreError::NotSupported);
        }
        let filesize = hex_u32(&header[54..62]) as usize;
        let namesize = hex_u32(&header[94..102]) as usize;
        if blob.len() - pos < 110 + namesize {
            return Err(StoreError::TooSmall);
        }
        let data_off = align4(pos + 110 + namesize);
        let name = &blob[pos + 110..pos + 110 + namesize - 1];
        if name == b"TRAILER!!!" {
            return Ok(entries);
        }
        if data_off + filesize > blob.len() {
            return Err(StoreError::TooSmall);
        }
        let data = &blob[data_off..data_off + filesize];
        entries.push(CpioEntry { name, data });
        pos = align4(data_off + filesize);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_KPKG: &[u8] = include_bytes!("../../../../tests/fixtures/kpkg/init.kpkg");

    #[test]
    fn parses_real_kpkg_entries() {
        let entries = parse_entries(REAL_KPKG).expect("parse real kpkg");
        assert_eq!(entries.len(), 2, "TRAILER 哨兵不应被返回");
        assert_eq!(entries[0].name, b"manifest");
        assert_eq!(entries[0].data, b"kcomp_smoke.kcomp\n");
        assert_eq!(entries[1].name, b"kcomp_smoke.kcomp");
        assert_eq!(entries[1].data.len(), 1016);
    }

    #[test]
    fn entries_are_zero_copy_slices() {
        let entries = parse_entries(REAL_KPKG).expect("parse real kpkg");
        let pos = entries[0].data.as_ptr() as usize - REAL_KPKG.as_ptr() as usize;
        assert_eq!(pos, 120, "data 切片应直接借用 blob 内部（零拷贝）");
    }
}
