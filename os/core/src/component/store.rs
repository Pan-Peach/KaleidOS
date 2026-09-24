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
#[derive(Debug, PartialEq, Eq)]
pub struct CpioEntry<'a> {
    pub name: &'a [u8],
    pub data: &'a [u8],
}

/// 解析 8 个 ASCII hex 字符为 u32。newc 头部字段都是 8 hex 位；
/// 非法字符（任意损坏输入的一部分）返回 `NotSupported`，绝不 panic。
fn hex_u32(bytes: &[u8]) -> Result<u32, StoreError> {
    let mut val = 0u32;
    for &b in bytes {
        let digit = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => return Err(StoreError::NotSupported),
        };
        val = val
            .checked_mul(16)
            .and_then(|v| v.checked_add(digit))
            .ok_or(StoreError::NotSupported)?;
    }
    Ok(val)
}

fn align4(n: usize) -> Result<usize, StoreError> {
    n.checked_add(3)
        .map(|n| n & !3)
        .ok_or(StoreError::NotSupported)
}

/// 解析 newc 归档：blob → 条目列表（纯内存逻辑，host-testable）。
///
/// 对**任意输入**只返回 `Ok` 或 `Err`，不会 panic：
/// - 头部/名字/数据区全部用 checked 算术定位；
/// - `namesize` 必须 ≥ 1（含末尾 NUL），否则 `-1` 会下溢；
/// - 非法 hex 字段返回 `NotSupported`；
/// - 截断返回 `TooSmall`。
pub fn parse_entries(blob: &'static [u8]) -> Result<Vec<CpioEntry<'static>>, StoreError> {
    let mut pos = 0usize;
    let mut entries = Vec::new();
    loop {
        let header_end = pos.checked_add(110).ok_or(StoreError::NotSupported)?;
        if header_end > blob.len() {
            return Err(StoreError::TooSmall);
        }
        let header = &blob[pos..header_end];
        if &header[..6] != b"070701" {
            return Err(StoreError::NotSupported);
        }
        let filesize = hex_u32(&header[54..62])? as usize;
        let namesize = hex_u32(&header[94..102])? as usize;
        if namesize == 0 {
            return Err(StoreError::NotSupported);
        }
        let name_end = pos
            .checked_add(110)
            .and_then(|end| end.checked_add(namesize))
            .ok_or(StoreError::NotSupported)?;
        if name_end > blob.len() {
            return Err(StoreError::TooSmall);
        }
        let name = &blob[pos + 110..name_end - 1];
        if name == b"TRAILER!!!" {
            return Ok(entries);
        }
        let data_off = align4(name_end)?;
        let data_end = data_off
            .checked_add(filesize)
            .ok_or(StoreError::NotSupported)?;
        if data_end > blob.len() {
            return Err(StoreError::TooSmall);
        }
        let data = &blob[data_off..data_end];
        entries.push(CpioEntry { name, data });
        // 下一个条目从对齐后的数据末尾开始；每次至少前进 110+1 字节，必然终止。
        pos = align4(data_end)?;
    }
}

/// 嵌入式仓库：持有 .initpkg 字节切片（无状态，blob 即一切）。
// blob 在 list/read 实现后读取
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
        let entries = parse_entries(self.blob)?
            .into_iter()
            .map(|e| StoreEntry {
                name: e.name.to_vec(),
                len: e.data.len(),
            })
            .collect();
        Ok(entries)
    }

    fn read(&self, name: &[u8], buf: &mut [u8]) -> Result<(), StoreError> {
        let entries = parse_entries(self.blob)?;
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .ok_or(StoreError::NotFound)?;
        if buf.len() < entry.data.len() {
            return Err(StoreError::TooSmall);
        }
        buf[..entry.data.len()].copy_from_slice(entry.data);
        Ok(())
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

// 这些用例需要 os/core/build.rs 生成的真实 `.kcomp` fixture（REAL_KPKG）；
// KALEIDOS_CORE_ONLY 下跳过组件构建，故用 `no_kcomp` 门控（其他测试照常运行）。
#[cfg(all(test, not(no_kcomp)))]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::vec;

    const REAL_KPKG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.kpkg"));

    // -- 手工构造 newc 归档（与 os/core/build.rs 的 write_newc 语义一致）----

    /// 手工拼一个 newc 条目（与 os/core/build.rs 的 write_newc 语义一致）：
    /// header + name(NUL 结尾) + 4 对齐 + data（含数据尾部对齐）。
    fn newc_raw(name: &str, data: &[u8]) -> Vec<u8> {
        let header = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            0,
            0o100644,
            0,
            0,
            1,
            0,
            data.len(),
            0,
            0,
            0,
            0,
            name.len() + 1,
            0
        );
        assert_eq!(header.len(), 110);
        let mut entry = Vec::new();
        entry.extend_from_slice(header.as_bytes());
        entry.extend_from_slice(name.as_bytes());
        entry.push(0);
        while !entry.len().is_multiple_of(4) {
            entry.push(0);
        }
        entry.extend_from_slice(data);
        while !entry.len().is_multiple_of(4) {
            entry.push(0);
        }
        entry
    }

    /// 拼一个完整归档（条目对齐 + trailer）。
    fn newc_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = Vec::new();
        for (name, data) in files {
            let raw = newc_raw(name, data);
            archive.extend_from_slice(&raw);
            while !archive.len().is_multiple_of(4) {
                archive.push(0);
            }
        }
        let raw = newc_raw("TRAILER!!!", &[]);
        archive.extend_from_slice(&raw);
        while !archive.len().is_multiple_of(4) {
            archive.push(0);
        }
        archive
    }

    /// 测试用：Vec → 'static 切片（泄漏到进程结束）。
    fn leak(v: Vec<u8>) -> &'static [u8] {
        Box::leak(v.into_boxed_slice())
    }

    fn align4_test(n: usize) -> usize {
        (n + 3) & !3
    }

    // -- 合法路径 -----------------------------------------------------------

    #[test]
    fn parses_real_kpkg_entries() {
        let entries = parse_entries(REAL_KPKG).expect("parse real kpkg");
        assert_eq!(entries.len(), 2, "TRAILER 哨兵不应被返回");
        assert_eq!(entries[0].name, b"manifest");
        assert_eq!(entries[0].data, b"kcomp_smoke.kcomp\n");
        assert_eq!(entries[1].name, b"kcomp_smoke.kcomp");
        assert!(!entries[1].data.is_empty());
    }

    #[test]
    fn entries_borrow_the_original_blob_zero_copy() {
        // 性质断言（不锁定 magic offset）：
        // 1) name/data 切片必须指向原 blob 内部（零拷贝，不是拷贝）；
        // 2) data 起点 = 按 newc 规则对齐后的名字末尾；
        // 3) data 内容与名字/长度字段一致。
        let entries = parse_entries(REAL_KPKG).expect("parse real kpkg");
        let blob_start = REAL_KPKG.as_ptr() as usize;
        let blob_end = blob_start + REAL_KPKG.len();

        for (i, entry) in entries.iter().enumerate() {
            let name_start = entry.name.as_ptr() as usize;
            let name_end = name_start + entry.name.len();
            let data_start = entry.data.as_ptr() as usize;
            let data_end = data_start + entry.data.len();
            assert!(
                blob_start <= name_start && name_end <= blob_end,
                "entry[{i}] name 必须在原 blob 内"
            );
            assert!(
                blob_start <= data_start && data_end <= blob_end,
                "entry[{i}] data 必须在原 blob 内"
            );
            // 零拷贝：data 紧跟 name（含 NUL），从 4 对齐处开始 —— 用对齐规则本身验证
            let expected_data_off = align4_test((name_start - blob_start) + entry.name.len() + 1);
            assert_eq!(
                data_start - blob_start,
                expected_data_off,
                "entry[{i}] data 偏移必须等于 newc 对齐规则"
            );
            assert!(data_start >= name_end, "data 不能与 name 重叠");
        }
    }

    #[test]
    fn parses_hand_built_archive_roundtrip() {
        let blob = leak(newc_archive(&[("a", b"hello"), ("b.bin", &[0u8; 64])]));
        let entries = parse_entries(blob).expect("parse hand-built archive");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, b"a");
        assert_eq!(entries[0].data, b"hello");
        assert_eq!(entries[1].name, b"b.bin");
        assert_eq!(entries[1].data.len(), 64);
        assert_eq!(entries[1].data, &[0u8; 64]);
    }

    #[test]
    fn trailer_only_archive_is_empty() {
        let blob = leak(newc_archive(&[]));
        let entries = parse_entries(blob).expect("trailer-only archive");
        assert!(entries.is_empty());
    }

    #[test]
    fn list_returns_directory_entries() {
        let store = EmbeddedStore::new(REAL_KPKG);
        let entries = store.list().expect("list real kpkg");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, b"manifest");
        assert_eq!(entries[0].len, 18);
        assert_eq!(entries[1].name, b"kcomp_smoke.kcomp");
        assert!(entries[1].len > 0);
    }

    #[test]
    fn read_manifest_returns_content() {
        let store = EmbeddedStore::new(REAL_KPKG);
        let mut buf = [0u8; 64];
        store.read(b"manifest", &mut buf).expect("read manifest");
        assert_eq!(&buf[..18], b"kcomp_smoke.kcomp\n");
    }

    #[test]
    fn read_missing_name_is_not_found() {
        let store = EmbeddedStore::new(REAL_KPKG);
        let mut buf = [0u8; 64];
        assert_eq!(store.read(b"nope", &mut buf), Err(StoreError::NotFound));
    }

    #[test]
    fn read_small_buffer_is_too_small() {
        let store = EmbeddedStore::new(REAL_KPKG);
        let mut buf = [0u8; 8];
        assert_eq!(
            store.read(b"kcomp_smoke.kcomp", &mut buf),
            Err(StoreError::TooSmall)
        );
    }

    // -- 对抗性输入：任意输入只允许 Ok / Err，不允许 panic ----------------

    #[test]
    fn empty_blob_is_too_small() {
        assert_eq!(parse_entries(&[]), Err(StoreError::TooSmall));
    }

    #[test]
    fn truncated_header_is_too_small() {
        let blob = leak(newc_raw("x", b"data")[..109].to_vec());
        assert_eq!(parse_entries(blob), Err(StoreError::TooSmall));
    }

    #[test]
    fn bad_magic_is_not_supported() {
        let mut raw = newc_raw("x", b"data");
        raw[..6].copy_from_slice(b"070700");
        assert_eq!(parse_entries(leak(raw)), Err(StoreError::NotSupported));
    }

    #[test]
    fn invalid_hex_field_is_not_supported() {
        // 名字长度字段填非法 hex（'z'），hex_u32 必须返回 Err 而非 panic。
        let mut raw = newc_raw("x", b"data");
        raw[94..102].copy_from_slice(b"zzzzzzzz");
        assert_eq!(parse_entries(leak(raw)), Err(StoreError::NotSupported));
    }

    #[test]
    fn invalid_hex_in_magic_adjacent_field_is_not_supported() {
        let mut raw = newc_raw("x", b"data");
        raw[54..62].copy_from_slice(b"00000g00");
        assert_eq!(parse_entries(leak(raw)), Err(StoreError::NotSupported));
    }

    #[test]
    fn zero_namesize_is_not_supported() {
        // namesize=0 时 `namesize - 1` 会下溢 —— parser 必须拒绝而非 panic。
        let mut raw = newc_raw("x", b"data");
        raw[94..102].copy_from_slice(b"00000000");
        assert_eq!(parse_entries(leak(raw)), Err(StoreError::NotSupported));
    }

    #[test]
    fn absurd_namesize_is_too_small() {
        let mut raw = newc_raw("x", b"data");
        raw[94..102].copy_from_slice(b"ffffffff");
        assert_eq!(parse_entries(leak(raw)), Err(StoreError::TooSmall));
    }

    #[test]
    fn absurd_filesize_is_too_small() {
        let mut raw = newc_raw("x", b"data");
        raw[54..62].copy_from_slice(b"ffffffff");
        assert_eq!(parse_entries(leak(raw)), Err(StoreError::TooSmall));
    }

    #[test]
    fn truncated_filename_is_too_small() {
        let mut raw = newc_raw("this-name-is-way-too-long-for-the-blob", b"data");
        raw[94..102].copy_from_slice(format!("{:08x}", 100).as_bytes());
        // blob 实际只到 ~118 字节，namesize=100 必然越界 → TooSmall
        assert_eq!(parse_entries(leak(raw)), Err(StoreError::TooSmall));
    }

    #[test]
    fn truncated_payload_is_too_small() {
        let mut raw = newc_raw("x", b"data");
        raw[54..62].copy_from_slice(format!("{:08x}", 1000).as_bytes());
        assert_eq!(parse_entries(leak(raw)), Err(StoreError::TooSmall));
    }

    #[test]
    fn missing_trailer_is_too_small() {
        // 合法条目 + 数据，但没有 TRAILER：下一轮循环头部不足 → TooSmall
        let blob = leak(newc_raw("x", b"data"));
        assert_eq!(parse_entries(blob), Err(StoreError::TooSmall));
    }

    #[test]
    fn trailing_garbage_after_valid_entry_is_not_supported() {
        // 合法条目（无 trailer）+ 足够长的垃圾字节（≥110 才能走到 magic 校验）
        let mut archive = newc_raw("x", b"data");
        archive.extend_from_slice(&[b'X'; 200]);
        assert_eq!(parse_entries(leak(archive)), Err(StoreError::NotSupported));
    }

    #[test]
    fn unaligned_payload_still_parses() {
        // namesize 让 name_end 落在非 4 对齐处：data 从对齐边界开始。
        // "abc" (4 字节含 NUL) → name_end=114 → data_off=116。
        let blob = leak(newc_archive(&[("abc", b"payload")]));
        let entries = parse_entries(blob).expect("unaligned entry parses");
        assert_eq!(entries[0].name, b"abc");
        assert_eq!(entries[0].data, b"payload");
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // 确定性遍历：各类恶意字节串都必须返回 Err（或极少数意外 Ok），不能 panic。
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x07; 109],
            vec![0x07; 110],
            vec![0xff; 256],
            vec![0x30; 1024], // 全 '0'
            b"070701".to_vec(),
            newc_raw("x", b"data"),
            vec![0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07],
        ];
        for case in cases.iter() {
            let blob = leak(case.clone());
            let _ = parse_entries(blob); // 唯一要求：不 panic
        }
    }

    // -- Property：任意字节输入永不 panic（docs/development/testing.md §2）----------------

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn arbitrary_blob_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let blob = leak(bytes);
            let _ = parse_entries(blob);
        }
    }

    proptest! {
        /// 对真实归档做随机字节翻转 + 截断，也不允许 panic。
        #[test]
        fn mutated_real_kpkg_never_panics(
            index in 0..REAL_KPKG.len(),
            truncate_to in 0..=REAL_KPKG.len(),
        ) {
            let mut mutated = REAL_KPKG.to_vec();
            mutated[index] ^= 0xFF;
            mutated.truncate(truncate_to);
            let blob = leak(mutated);
            let _ = parse_entries(blob);
        }
    }
}
