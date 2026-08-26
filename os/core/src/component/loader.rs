//! 组件加载器（Linux insmod 的最小版）：解析 ELF ET_REL → 放段 → 找入口符号。
//!
//! 第一版约定（诚实边界）：
//! - 组件必须无重定位需求（kcomp 不引用未定义符号/内核函数）；
//!   遇到重定位段（SHT_RELA/SHT_REL）直接拒绝（UnsupportedRelocation）
//! - 只放段 .text（第一个 PROGBITS），假设单代码段（教学阶段简化）
//! - 重定位/多段/内核符号表都是第二版（组件调用 printk 时再做）

use crate::memory;
use alloc::vec::Vec;

/// 加载失败原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderError {
    BadMagic,
    UnsupportedFormat,
    NotRelocatable,
    MachineMismatch,
    NoTextSection,
    NoEntrySymbol,
    UnsupportedRelocation,
    OutOfMemory,
}

/// 加载完成的组件：代码已在内存中，入口已定位（未调用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedComponent {
    pub base: usize,
    pub entry: usize,
    pub text_size: usize,
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

fn parse_elf_header(blob: &[u8]) -> Result<(usize, usize), LoaderError> {
    if blob.len() < 64 || &blob[..4] != b"\x7fELF" {
        return Err(LoaderError::BadMagic);
    }
    if blob[4] != 2 || blob[5] != 1 {
        return Err(LoaderError::UnsupportedFormat);
    }
    if u16_at(blob, 16) != 1 {
        return Err(LoaderError::NotRelocatable);
    }
    let e_shoff = u64_at(blob, 40) as usize;
    let e_shnum = u16_at(blob, 60) as usize;
    Ok((e_shoff, e_shnum))
}

/// 解析 ET_REL + 放段 + 定位入口（不调用）。
/// `expected_machine` 来自 arch（ELF_MACHINE），loader 本身机器无关。
pub fn load_component(blob: &[u8], expected_machine: u16) -> Result<LoadedComponent, LoaderError> {
    let (e_shoff, e_shnum) = parse_elf_header(blob)?;
    if e_shoff + e_shnum * 64 > blob.len() {
        return Err(LoaderError::UnsupportedFormat);
    }

    let machine = u16_at(blob, 18);
    if machine != expected_machine {
        return Err(LoaderError::MachineMismatch);
    }

    // 段表收集：(type, offset, size, link, flags)
    let mut sections: Vec<(u32, usize, usize, usize, u64)> = Vec::new();
    for i in 0..e_shnum {
        let sh = &blob[e_shoff + i * 64..e_shoff + i * 64 + 64];
        let ty = u32_at(sh, 4);
        let off = u64_at(sh, 24) as usize;
        let size = u64_at(sh, 32) as usize;
        let link = u32_at(sh, 40) as usize;
        let flags = u64_at(sh, 8);
        if off + size > blob.len() {
            return Err(LoaderError::UnsupportedFormat);
        }
        sections.push((ty, off, size, link, flags));
    }

    // 遇到重定位段即拒绝（第一版边界）
    let has_reloc = sections.iter().any(|s| s.0 == 4 || s.0 == 9);
    if has_reloc {
        return Err(LoaderError::UnsupportedRelocation);
    }

    // 可执行段（PROGBITS + AX 标志）：Rust 按函数分片（.text / .text.kcomp_init ...），
    // 要全部收齐、按顺序放置，入口符号按 st_shndx 定位到具体段
    let exec_segs: Vec<(usize, usize, usize)> = sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.0 == 1 && (s.4 & 0x6) == 0x6)
        .map(|(i, s)| (i, s.1, s.2))
        .collect();
    if exec_segs.is_empty() {
        return Err(LoaderError::NoTextSection);
    }

    // 放置布局：段按序 4 对齐叠加 → (shndx, put_offset)
    let mut put = 0usize;
    let mut seg_place: Vec<(usize, usize)> = Vec::new();
    for &(idx, _, size) in &exec_segs {
        put = (put + 3) & !3;
        seg_place.push((idx, put));
        put += size;
    }
    let code_size = put;

    // 查找 2：符号表（SYMTAB）+ 它的字符串表（sh_link）
    let (_, sym_off, sym_size, sym_link, _) = *sections
        .iter()
        .find(|s| s.0 == 2)
        .ok_or(LoaderError::NoEntrySymbol)?;
    if sym_link >= e_shnum {
        return Err(LoaderError::UnsupportedFormat);
    }
    let (_, str_off, _, _, _) = sections[sym_link];

    // 查找 3：kcomp_init（FUNC；st_value + st_shndx 段内偏移）
    let mut entry_off: Option<(usize, usize)> = None;
    for j in 0..sym_size / 24 {
        let sym = &blob[sym_off + j * 24..sym_off + j * 24 + 24];
        if sym[4] & 0x0f != 2 {
            continue;
        }
        let name_off = u32_at(sym, 0) as usize;
        let name_start = str_off + name_off;
        let name_end = blob[name_start..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| name_start + p)
            .ok_or(LoaderError::UnsupportedFormat)?;
        if &blob[name_start..name_end] == b"kcomp_init" {
            let seg_idx = u16_at(sym, 6) as usize;
            entry_off = Some((seg_idx, u64_at(sym, 8) as usize));
            break;
        }
    }
    let (entry_seg, entry_seg_off) = entry_off.ok_or(LoaderError::NoEntrySymbol)?;
    let entry_put = seg_place
        .iter()
        .find(|(idx, _)| *idx == entry_seg)
        .ok_or(LoaderError::NoEntrySymbol)?
        .1;

    // 放段：逐帧分配（物理地址 = 恒等映射），按布局拷贝所有执行段
    let frames = code_size.div_ceil(memory::FRAME_SIZE);
    let mut fids = Vec::new();
    for _ in 0..frames {
        let fid = memory::alloc_frame().map_err(|_| LoaderError::OutOfMemory)?;
        fids.push(fid);
    }
    let base = fids[0].start_pa();
    for &(idx, put) in &seg_place {
        let (_, off, size, _, _) = sections[idx];
        let dst = unsafe { core::slice::from_raw_parts_mut((base + put) as *mut u8, size) };
        dst.copy_from_slice(&blob[off..off + size]);
    }

    Ok(LoadedComponent {
        base,
        entry: base + entry_put + entry_seg_off,
        text_size: code_size,
    })
}

/// 调用组件入口（insmod 的 init 调用）。返回组件自身的结果码（0 = 成功）。
pub fn call_init(comp: &LoadedComponent) -> i32 {
    let init: extern "C" fn() -> i32 = unsafe { core::mem::transmute(comp.entry) };
    init()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORETEST_KCOMP: &[u8] = include_bytes!("../../../../tests/fixtures/kpkg/core_test.kcomp");

    #[test]
    fn parses_header_of_core_test() {
        let (e_shoff, e_shnum) = parse_elf_header(CORETEST_KCOMP).expect("parse header");
        assert_eq!(e_shoff, 504);
        assert_eq!(e_shnum, 8);
    }

    #[test]
    fn loads_core_test_component() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let comp = load_component(CORETEST_KCOMP, 0xF3).expect("load core_test.kcomp");
        assert_eq!(comp.entry - comp.base, 0, "kcomp_init 位于放置段映射起点");
        assert!(comp.text_size >= 4);
    }

    #[test]
    fn rejects_wrong_machine() {
        assert_eq!(
            load_component(CORETEST_KCOMP, 0x3E),
            Err(LoaderError::MachineMismatch)
        );
    }

    #[test]
    fn rejects_bad_magic() {
        assert_eq!(
            load_component(b"not an elf", 0xF3),
            Err(LoaderError::BadMagic)
        );
    }
}
