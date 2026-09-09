//! 组件加载器（Linux insmod 的最小版）：解析 ELF ET_REL → 放段 → 找入口符号。
//!
//! 当前约定（诚实边界）：
//! - 接受 RV32/ELF32 与 RV64/ELF64 的 ET_REL 组件；
//! - 只放置 ALLOC 内容段，按段顺序组成一个连续组件映像；
//! - 支持组件当前实际产生的 RISC-V CALL/PCREL 与 32/64-bit data relocation。

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

/* 重定位：只支持组件实际产生的最小集（CALL/PCREL + 32/64-bit data），
其余类型诚实拒绝（UnsupportedRelocation），待未来扩展。 */
const R_RISCV_32: u32 = 1;
const R_RISCV_64: u32 = 2;
const R_RISCV_CALL: u32 = 18;
const R_RISCV_CALL_PLT: u32 = 19;
const R_RISCV_PCREL_HI20: u32 = 23;
const R_RISCV_PCREL_LO12_I: u32 = 24;
const R_RISCV_HI20: u32 = 26;
const R_RISCV_LO12_I: u32 = 27;
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

#[derive(Clone, Copy)]
enum ElfClass {
    Bits32,
    Bits64,
}

#[derive(Clone, Copy)]
struct ElfHeader {
    class: ElfClass,
    section_offset: usize,
    section_size: usize,
    section_count: usize,
}

#[derive(Clone, Copy)]
struct Section {
    ty: u32,
    offset: usize,
    size: usize,
    link: usize,
    flags: u64,
    info: usize,
}

#[derive(Clone, Copy)]
struct Symbol {
    name: usize,
    shndx: usize,
    value: u64,
}

fn parse_elf_header(blob: &[u8]) -> Result<ElfHeader, LoaderError> {
    if blob.len() < 20 || &blob[..4] != b"\x7fELF" {
        return Err(LoaderError::BadMagic);
    }
    let class = match blob[4] {
        1 => ElfClass::Bits32,
        2 => ElfClass::Bits64,
        _ => return Err(LoaderError::UnsupportedFormat),
    };
    if blob[5] != 1 {
        return Err(LoaderError::UnsupportedFormat);
    }
    if u16_at(blob, 16) != 1 {
        return Err(LoaderError::NotRelocatable);
    }

    let (header_size, section_offset, section_size, section_count) = match class {
        ElfClass::Bits32 => (
            52,
            u32_at(blob, 32) as usize,
            u16_at(blob, 46) as usize,
            u16_at(blob, 48) as usize,
        ),
        ElfClass::Bits64 => (
            64,
            u64_at(blob, 40) as usize,
            u16_at(blob, 58) as usize,
            u16_at(blob, 60) as usize,
        ),
    };
    if blob.len() < header_size
        || section_size
            != match class {
                ElfClass::Bits32 => 40,
                ElfClass::Bits64 => 64,
            }
    {
        return Err(LoaderError::UnsupportedFormat);
    }
    let table_size = section_size
        .checked_mul(section_count)
        .ok_or(LoaderError::UnsupportedFormat)?;
    section_offset
        .checked_add(table_size)
        .filter(|&end| end <= blob.len())
        .ok_or(LoaderError::UnsupportedFormat)?;
    Ok(ElfHeader {
        class,
        section_offset,
        section_size,
        section_count,
    })
}

fn parse_sections(blob: &[u8], header: ElfHeader) -> Result<Vec<Section>, LoaderError> {
    let mut sections = Vec::new();
    for i in 0..header.section_count {
        let off = header
            .section_offset
            .checked_add(
                i.checked_mul(header.section_size)
                    .ok_or(LoaderError::UnsupportedFormat)?,
            )
            .ok_or(LoaderError::UnsupportedFormat)?;
        let sh = &blob[off..off + header.section_size];
        let section = match header.class {
            ElfClass::Bits32 => Section {
                ty: u32_at(sh, 4),
                offset: u32_at(sh, 16) as usize,
                size: u32_at(sh, 20) as usize,
                link: u32_at(sh, 24) as usize,
                flags: u32_at(sh, 8) as u64,
                info: u32_at(sh, 28) as usize,
            },
            ElfClass::Bits64 => Section {
                ty: u32_at(sh, 4),
                offset: u64_at(sh, 24) as usize,
                size: u64_at(sh, 32) as usize,
                link: u32_at(sh, 40) as usize,
                flags: u64_at(sh, 8),
                info: u32_at(sh, 44) as usize,
            },
        };
        section
            .offset
            .checked_add(section.size)
            .filter(|&end| end <= blob.len())
            .ok_or(LoaderError::UnsupportedFormat)?;
        sections.push(section);
    }
    Ok(sections)
}

fn symbol_at(blob: &[u8], offset: usize, class: ElfClass) -> Result<Symbol, LoaderError> {
    match class {
        ElfClass::Bits32 => {
            let sym = blob
                .get(offset..offset + 16)
                .ok_or(LoaderError::UnsupportedFormat)?;
            Ok(Symbol {
                name: u32_at(sym, 0) as usize,
                shndx: u16_at(sym, 14) as usize,
                value: u32_at(sym, 4) as u64,
            })
        }
        ElfClass::Bits64 => {
            let sym = blob
                .get(offset..offset + 24)
                .ok_or(LoaderError::UnsupportedFormat)?;
            Ok(Symbol {
                name: u32_at(sym, 0) as usize,
                shndx: u16_at(sym, 6) as usize,
                value: u64_at(sym, 8),
            })
        }
    }
}

/// 解析 ET_REL + 放段 + 定位入口（不调用）。
/// `expected_machine` 来自 arch（ELF_MACHINE），loader 本身机器无关。
pub fn load_component(blob: &[u8], expected_machine: u16) -> Result<LoadedComponent, LoaderError> {
    let header = parse_elf_header(blob)?;

    let machine = u16_at(blob, 18);
    if machine != expected_machine {
        return Err(LoaderError::MachineMismatch);
    }

    let sections = parse_sections(blob, header)?;

    // 重定位段（SHT_RELA=4）：收集 (目标段, off, size, symtab_off, symtab_size, strtab_off)。
    // SHT_REL (9) 不支持（rustc 输出 RELA；兼容检查保持诚实拒绝）。
    let mut relas: Vec<(usize, usize, usize, usize, usize, usize)> = Vec::new();
    for s in &sections {
        if s.ty == 4 {
            if s.link >= header.section_count || s.info >= header.section_count {
                return Err(LoaderError::UnsupportedFormat);
            }
            let symtab = sections[s.link];
            let sym_link = symtab.link;
            if sym_link >= header.section_count {
                return Err(LoaderError::UnsupportedFormat);
            }
            let strtab = sections[sym_link];
            relas.push((
                s.info,
                s.offset,
                s.size,
                symtab.offset,
                symtab.size,
                strtab.offset,
            ));
        } else if s.ty == 9 {
            return Err(LoaderError::UnsupportedRelocation);
        }
    }

    // ALLOC 内容段（PROGBITS + ALLOC 标志）：AX（代码 .text*）与 A（.rodata/.data）都放置。
    // 含 0-size 段（如带头 .text 空壳）保持段序，重定位 target 按 seg_place 定位。
    let place_segs: Vec<(usize, usize, usize)> = sections
        .iter()
        .enumerate()
        .filter(|(_, s)| s.ty == 1 && (s.flags & 0x2) == 0x2)
        .map(|(i, s)| (i, s.offset, s.size))
        .collect();
    if place_segs.is_empty() {
        return Err(LoaderError::NoTextSection);
    }

    // 放置布局：段按序 4 对齐叠加 → (shndx, put_offset)
    let mut put = 0usize;
    let mut seg_place: Vec<(usize, usize)> = Vec::new();
    for &(idx, _, size) in &place_segs {
        put = put.checked_add(3).ok_or(LoaderError::UnsupportedFormat)? & !3;
        seg_place.push((idx, put));
        put = put
            .checked_add(size)
            .ok_or(LoaderError::UnsupportedFormat)?;
    }
    let code_size = put;

    // 查找 2：符号表（SYMTAB）+ 它的字符串表（sh_link）
    let symtab_index = sections
        .iter()
        .position(|s| s.ty == 2)
        .ok_or(LoaderError::NoEntrySymbol)?;
    let symtab = sections[symtab_index];
    if symtab.link >= header.section_count {
        return Err(LoaderError::UnsupportedFormat);
    }
    let strtab = sections[symtab.link];
    let symbol_size = match header.class {
        ElfClass::Bits32 => 16,
        ElfClass::Bits64 => 24,
    };

    // 查找 3：kcomp_init（FUNC；st_value + st_shndx 段内偏移）
    let mut entry_off: Option<(usize, usize)> = None;
    for j in 0..symtab.size / symbol_size {
        let symbol = symbol_at(blob, symtab.offset + j * symbol_size, header.class)?;
        let sym = &blob[symtab.offset + j * symbol_size..symtab.offset + (j + 1) * symbol_size];
        if sym[match header.class {
            ElfClass::Bits32 => 12,
            ElfClass::Bits64 => 4,
        }] & 0x0f
            != 2
        {
            continue;
        }
        let name = cstr_at(
            blob,
            strtab
                .offset
                .checked_add(symbol.name)
                .ok_or(LoaderError::UnsupportedFormat)?,
        )?;
        if name == b"kcomp_init" {
            let value =
                usize::try_from(symbol.value).map_err(|_| LoaderError::UnsupportedFormat)?;
            entry_off = Some((symbol.shndx, value));
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
        let section = sections[idx];
        let off = section.offset;
        let size = section.size;
        let dst = unsafe { core::slice::from_raw_parts_mut((base + put) as *mut u8, size) };
        dst.copy_from_slice(&blob[off..off + size]);
    }

    // 放段完成后应用重定位：UNDEF 符号查导出表（白名单），组件内符号按放置定位。
    apply_relocations(blob, base, &seg_place, &relas, header.class)?;

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
    class: ElfClass,
) -> Result<(), LoaderError> {
    let rela_size = match class {
        ElfClass::Bits32 => 12,
        ElfClass::Bits64 => 24,
    };
    let symbol_size = match class {
        ElfClass::Bits32 => 16,
        ElfClass::Bits64 => 24,
    };
    // PCREL_HI20 缓存：(target_shndx, r_in_seg) → (S + A - P_hi)，供配套 LO12_I 查询。
    let mut hi_cache: Vec<((usize, usize), i64)> = Vec::new();
    for &(target, r_off, r_size, sym_off, sym_size, str_off) in relas {
        let Some(&(_, target_put)) = seg_place.iter().find(|(i, _)| *i == target) else {
            return Err(LoaderError::UnsupportedRelocation);
        };
        if r_size % rela_size != 0 || sym_size % symbol_size != 0 {
            return Err(LoaderError::UnsupportedFormat);
        }
        for j in 0..r_size / rela_size {
            let e = &blob[r_off + j * rela_size..r_off + (j + 1) * rela_size];
            let (r_in_seg, sym_idx, rtype, r_addend) = match class {
                ElfClass::Bits32 => {
                    let r_info = u32_at(e, 4);
                    (
                        u32_at(e, 0) as usize,
                        (r_info >> 8) as usize,
                        r_info & 0xff,
                        u32_at(e, 8) as i32 as i64,
                    )
                }
                ElfClass::Bits64 => {
                    let r_info = u64_at(e, 8);
                    (
                        u64_at(e, 0) as usize,
                        (r_info >> 32) as usize,
                        (r_info & 0xffff_ffff) as u32,
                        u64_at(e, 16) as i64,
                    )
                }
            };
            if rtype == R_RISCV_RELAX || rtype == 0 {
                continue;
            }
            if sym_idx >= sym_size / symbol_size {
                return Err(LoaderError::UnsupportedFormat);
            }
            let symbol = symbol_at(blob, sym_off + sym_idx * symbol_size, class)?;
            let st_shndx = symbol.shndx;
            let st_value = symbol.value as i64;

            let s_addr: i64 = if st_shndx == SHN_UNDEF {
                let name = cstr_at(
                    blob,
                    str_off
                        .checked_add(symbol.name)
                        .ok_or(LoaderError::UnsupportedFormat)?,
                )?;
                let addr =
                    crate::component::export::resolve(name).ok_or(LoaderError::UnresolvedSymbol)?;
                // Early boot keeps the RAM identity-mapped, so the low alias
                // of a high-half kernel symbol is reachable from a
                // low-address component (auipc+jalr covers only ±2 GiB).
                // Host builds offset nothing.
                arch::physical_address_of(addr) as i64
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
                R_RISCV_HI20 => {
                    let v = s_addr + r_addend;
                    let imm20 = ((v + 0x800) >> 12) & 0xFFFFF;
                    let mut lui = unsafe { (loc as *mut u32).read() };
                    lui = (imm20 as u32) << 12 | (lui & 0xF80) | 0x37;
                    unsafe {
                        (loc as *mut u32).write(lui);
                    }
                }
                R_RISCV_PCREL_LO12_I => {
                    let v_hi = hi_cache
                        .iter()
                        .find(|&(k, _)| *k == (st_shndx, st_value as usize))
                        .map(|(_, v)| *v)
                        .ok_or(LoaderError::UnsupportedFormat)?;
                    // Low 12 bits of the same hi20 value; bit 11 makes the
                    // immediate negative once sign-extended by the CPU, and
                    // HI20 already rounded up (v + 0x800) >> 12.
                    let imm12 = (v_hi & 0xFFF) as u32;
                    let mut insn = unsafe { (loc as *mut u32).read() };
                    insn = (imm12 << 20) | (insn & 0x000F_FFFF);
                    unsafe {
                        (loc as *mut u32).write(insn);
                    }
                }
                R_RISCV_LO12_I => {
                    let v = s_addr + r_addend;
                    let imm12 = (v & 0xFFF) as u32;
                    let mut insn = unsafe { (loc as *mut u32).read() };
                    insn = (imm12 << 20) | (insn & 0x000F_FFFF);
                    unsafe {
                        (loc as *mut u32).write(insn);
                    }
                }
                R_RISCV_32 => {
                    if !matches!(class, ElfClass::Bits32) {
                        return Err(LoaderError::UnsupportedRelocation);
                    }
                    let v = s_addr + r_addend;
                    unsafe {
                        (loc as *mut u32).write(v as u32);
                    }
                }
                R_RISCV_64 => {
                    if !matches!(class, ElfClass::Bits64) {
                        return Err(LoaderError::UnsupportedRelocation);
                    }
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
        let header = parse_elf_header(CORETEST_KCOMP).expect("parse header");
        // 布局随编译器/组件内容变化，只锚定 sanity（非零节区偏移 + 足量节区）。
        assert!(header.section_offset > 0);
        assert!(header.section_count >= 4);
    }

    #[test]
    fn loads_core_test_component() {
        let _g = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        let comp = load_component(CORETEST_KCOMP, 0xF3).expect("load core_test.kcomp");
        assert!(comp.entry >= comp.base, "kcomp_init 必须位于放置段映射内");
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
