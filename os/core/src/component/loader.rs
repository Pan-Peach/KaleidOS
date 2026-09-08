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
    UnresolvedSymbol,
    OutOfMemory,
}

/* 重定位：只支持组件实际产生的最小集（R_RISCV_CALL 对 + 忽略 RELAX），
其余类型诚实拒绝（UnsupportedRelocation），待未来扩展。 */
const R_RISCV_64: u32 = 2;
const R_RISCV_CALL: u32 = 18;
const R_RISCV_CALL_PLT: u32 = 19;
const R_RISCV_PCREL_HI20: u32 = 23;
const R_RISCV_PCREL_LO12_I: u32 = 24;
const R_RISCV_RELAX: u32 = 51;
const SHN_UNDEF: usize = 0;

/// 加载完成的组件：代码已在内存中，入口已定位（未调用）。
#[derive(Debug, PartialEq, Eq)]
pub struct LoadedComponent {
    pub base: usize,
    pub entry: usize,
    pub text_size: usize,
    pub(crate) memory: Option<memory::MemoryLease>,
}

impl LoadedComponent {
    pub(crate) fn take_memory(&mut self) -> Option<memory::MemoryLease> {
        self.memory.take()
    }
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

    // 段表收集：(type, offset, size, link, flags, info)
    let mut sections: Vec<(u32, usize, usize, usize, u64, usize)> = Vec::new();
    for i in 0..e_shnum {
        let sh = &blob[e_shoff + i * 64..e_shoff + i * 64 + 64];
        let ty = u32_at(sh, 4);
        let off = u64_at(sh, 24) as usize;
        let size = u64_at(sh, 32) as usize;
        let link = u32_at(sh, 40) as usize;
        let flags = u64_at(sh, 8);
        let info = u32_at(sh, 44) as usize;
        if off + size > blob.len() {
            return Err(LoaderError::UnsupportedFormat);
        }
        sections.push((ty, off, size, link, flags, info));
    }

    // 重定位段（SHT_RELA=4）：收集 (目标段, off, size, symtab_off, symtab_size, strtab_off)。
    // SHT_REL (9) 不支持（rustc 输出 RELA；兼容检查保持诚实拒绝）。
    let mut relas: Vec<(usize, usize, usize, usize, usize, usize)> = Vec::new();
    for s in &sections {
        if s.0 == 4 {
            if s.3 >= e_shnum || s.5 >= e_shnum {
                return Err(LoaderError::UnsupportedFormat);
            }
            let (_, sym_off, sym_size, sym_link, _, _) = sections[s.3];
            if sym_link >= e_shnum {
                return Err(LoaderError::UnsupportedFormat);
            }
            let (_, str_off, _, _, _, _) = sections[sym_link];
            relas.push((s.5, s.1, s.2, sym_off, sym_size, str_off));
        } else if s.0 == 9 {
            return Err(LoaderError::UnsupportedRelocation);
        }
    }

    // ALLOC 内容段（PROGBITS + ALLOC 标志）：AX（代码 .text*）与 A（.rodata/.data）都放置。
    // 含 0-size 段（如带头 .text 空壳）保持段序，重定位 target 按 seg_place 定位。
    let place_segs: Vec<(usize, usize, usize)> = sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.0 == 1 && (s.4 & 0x2) == 0x2)
        .map(|(i, s)| (i, s.1, s.2))
        .collect();
    if place_segs.is_empty() {
        return Err(LoaderError::NoTextSection);
    }

    // 放置布局：段按序 4 对齐叠加 → (shndx, put_offset)
    let mut put = 0usize;
    let mut seg_place: Vec<(usize, usize)> = Vec::new();
    for &(idx, _, size) in &place_segs {
        put = (put + 3) & !3;
        seg_place.push((idx, put));
        put += size;
    }
    let code_size = put;

    // 查找 2：符号表（SYMTAB）+ 它的字符串表（sh_link）
    let (_, sym_off, sym_size, sym_link, _, _) = *sections
        .iter()
        .find(|s| s.0 == 2)
        .ok_or(LoaderError::NoEntrySymbol)?;
    if sym_link >= e_shnum {
        return Err(LoaderError::UnsupportedFormat);
    }
    let (_, str_off, _, _, _, _) = sections[sym_link];

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

    // 放段：一次分配连续区域（物理地址 = 恒等映射），按布局拷贝所有执行段。
    let image_memory = memory::alloc_region(code_size).map_err(|_| LoaderError::OutOfMemory)?;
    let base = image_memory.region().base;
    for &(idx, put) in &seg_place {
        let (_, off, size, _, _, _) = sections[idx];
        let dst = unsafe { core::slice::from_raw_parts_mut((base + put) as *mut u8, size) };
        dst.copy_from_slice(&blob[off..off + size]);
    }

    // 放段完成后应用重定位：UNDEF 符号查导出表（白名单），组件内符号按放置定位。
    apply_relocations(blob, base, &seg_place, &relas)?;

    Ok(LoadedComponent {
        base,
        entry: base + entry_put + entry_seg_off,
        text_size: code_size,
        memory: Some(image_memory),
    })
}

/// 调用组件入口（insmod 的 init 调用）。返回组件自身的结果码（0 = 成功）。
pub fn call_init(comp: &LoadedComponent) -> i32 {
    let init: extern "C" fn() -> i32 = unsafe { core::mem::transmute(comp.entry) };
    init()
}

fn cstr_at(blob: &[u8], off: usize) -> Result<&[u8], LoaderError> {
    if off >= blob.len() {
        return Err(LoaderError::UnsupportedFormat);
    }
    let end = blob[off..]
        .iter()
        .position(|&b| b == 0)
        .ok_or(LoaderError::UnsupportedFormat)?;
    Ok(&blob[off..off + end])
}

/// 应用重定位：修改已放置的内存（base + seg_place 为绝对地址基准）。
/// - UNDEF 符号：查导出表（白名单）→ 不存在 = UnresolvedSymbol（整次加载失败）；
/// - 组件内定义符号：按 seg_place 定位（名称仅用于入口查找，解析不需要名字）；
/// - 支持 R_RISCV_CALL / R_RISCV_CALL_PLT（auipc + jalr 对）；RELAX 条目忽略；
/// - 其余类型 UnsupportedRelocation（诚实边界，未来扩展）。
fn apply_relocations(
    blob: &[u8],
    base: usize,
    seg_place: &[(usize, usize)],
    relas: &[(usize, usize, usize, usize, usize, usize)],
) -> Result<(), LoaderError> {
    // PCREL_HI20 缓存：(target_shndx, r_in_seg) → (S + A - P_hi)，供配套 LO12_I 查询。
    let mut hi_cache: Vec<((usize, usize), i64)> = Vec::new();
    for &(target, r_off, r_size, sym_off, sym_size, str_off) in relas {
        let Some(&(_, target_put)) = seg_place.iter().find(|(i, _)| *i == target) else {
            return Err(LoaderError::UnsupportedRelocation);
        };
        for j in 0..r_size / 24 {
            let e = &blob[r_off + j * 24..r_off + j * 24 + 24];
            let r_in_seg = u64_at(e, 0) as usize;
            let r_info = u64_at(e, 8);
            let r_addend = u64_at(e, 16) as i64;
            let sym_idx = (r_info >> 32) as usize;
            let rtype = (r_info & 0xffff_ffff) as u32;
            if rtype == R_RISCV_RELAX || rtype == 0 {
                continue;
            }
            if sym_idx >= sym_size / 24 {
                return Err(LoaderError::UnsupportedFormat);
            }
            let sym = &blob[sym_off + sym_idx * 24..sym_off + sym_idx * 24 + 24];
            let st_shndx = u16_at(sym, 6) as usize;
            let st_value = u64_at(sym, 8) as i64;

            let s_addr: i64 = if st_shndx == SHN_UNDEF {
                let name = cstr_at(blob, str_off + u32_at(sym, 0) as usize)?;
                let addr =
                    crate::component::export::resolve(name).ok_or(LoaderError::UnresolvedSymbol)?;
                addr as i64
            } else {
                let (_, put) = seg_place
                    .iter()
                    .find(|(i, _)| *i == st_shndx)
                    .ok_or(LoaderError::UnsupportedFormat)?;
                (base + put) as i64 + st_value
            };

            let loc = base + target_put + r_in_seg;
            match rtype {
                R_RISCV_CALL | R_RISCV_CALL_PLT => {
                    let v = s_addr + r_addend - loc as i64;
                    // auipc+jalr 覆盖 ±2 GiB（imm20<<12 + imm12）；超出即跳转不可达。
                    if !(-(1 << 31)..(1 << 31)).contains(&v) {
                        return Err(LoaderError::UnsupportedRelocation);
                    }
                    let imm20 = ((v + 0x800) >> 12) & 0xFFFFF;
                    let imm12 = (v & 0xFFF) as u32;
                    let mut auipc = unsafe { (loc as *mut u32).read() };
                    auipc = (imm20 as u32) << 12 | (auipc & 0xF80) | 0x17;
                    let mut jalr = unsafe { ((loc + 4) as *mut u32).read() };
                    jalr = (imm12 << 20) | (jalr & 0x000F_FFFF);
                    unsafe {
                        (loc as *mut u32).write(auipc);
                        ((loc + 4) as *mut u32).write(jalr);
                    }
                }
                R_RISCV_PCREL_HI20 => {
                    let v = s_addr + r_addend - loc as i64;
                    hi_cache.push(((target, r_in_seg), v));
                    let imm20 = ((v + 0x800) >> 12) & 0xFFFFF;
                    let mut auipc = unsafe { (loc as *mut u32).read() };
                    auipc = (imm20 as u32) << 12 | (auipc & 0xF80) | 0x17;
                    unsafe {
                        (loc as *mut u32).write(auipc);
                    }
                }
                R_RISCV_PCREL_LO12_I => {
                    let v_hi = hi_cache
                        .iter()
                        .find(|&(k, _)| *k == (st_shndx, st_value as usize))
                        .map(|(_, v)| *v)
                        .ok_or(LoaderError::UnsupportedFormat)?;
                    let imm12 = ((v_hi - 4) & 0xFFF) as u32;
                    let mut insn = unsafe { (loc as *mut u32).read() };
                    insn = (imm12 << 20) | (insn & 0x000F_FFFF);
                    unsafe {
                        (loc as *mut u32).write(insn);
                    }
                }
                R_RISCV_64 => {
                    let v = s_addr + r_addend;
                    unsafe {
                        (loc as *mut u64).write(v as u64);
                    }
                }
                _ => return Err(LoaderError::UnsupportedRelocation),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORETEST_KCOMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/core_test.kcomp"));
    const SMOKE_KCOMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kcomp_smoke.kcomp"));
    const SMOKE_MIN_KCOMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/smoke_min.kcomp"));

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

    #[test]
    fn loads_smoke_with_relocations() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let comp = load_component(SMOKE_KCOMP, 0xF3).expect("load smoke.kcomp");
        assert!(comp.entry >= comp.base);
        assert!(comp.text_size > 0);
    }

    #[test]
    fn rejects_unknown_symbol() {
        // 破坏 strtab 中的导出符号名（保留长度）→ 重定位参考未知符号 → 整次加载失败
        let mut patched = SMOKE_KCOMP.to_vec();
        let name = b"kcore_console_write_byte";
        let pos = patched
            .windows(name.len())
            .position(|w| w == name)
            .expect("export name present in strtab");
        patched[pos] = b'x';
        assert_eq!(
            load_component(&patched, 0xF3),
            Err(LoaderError::UnresolvedSymbol)
        );
    }

    #[test]
    fn relocation_writes_correct_call_target() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let comp = load_component(SMOKE_MIN_KCOMP, 0xF3).expect("load smoke_min.kcomp");
        // 极简组件第一个 CALL：r_offset=0x8（auipc）+0xc（jalr），target=text 段 put=0
        let a = unsafe { core::ptr::read((comp.base + 8) as *const u32) };
        let j = unsafe { core::ptr::read((comp.base + 12) as *const u32) };
        assert_eq!(a & 0x7F, 0x17, "auipc opcode");
        assert_eq!(j & 0x7F, 0x67, "jalr opcode");
        assert_eq!((j >> 15) & 0x1F, 1, "jalr rs1=ra");
        assert_eq!((j >> 7) & 0x1F, 1, "jalr rd=ra");
        let mut imm20 = ((a >> 12) & 0xFFFFF) as i64;
        if imm20 >= 1 << 19 {
            imm20 -= 1 << 20;
        }
        let mut imm12 = ((j >> 20) & 0xFFF) as i64;
        if imm12 >= 1 << 11 {
            imm12 -= 1 << 12;
        }
        let target = (comp.base as i64 + 8) + (imm20 << 12) + imm12;
        let expected =
            crate::component::export::resolve(b"kcore_console_write_byte").unwrap() as i64;
        assert_eq!(target, expected, "重定位写入的目标必须等于 resolve 地址");
    }
}
