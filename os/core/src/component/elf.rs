//! Minimal, architecture-neutral ELF ET_REL object parser.
//!
//! This module knows the ELF file layout only.  It does not interpret
//! `e_machine`, relocation kinds, instruction encodings, or component policy.

use alloc::vec::Vec;

const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_RELA: u32 = 4;
const SHT_NOBITS: u32 = 8;
const SHT_REL: u32 = 9;
const SHF_ALLOC: u64 = 0x2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElfError {
    BadMagic,
    UnsupportedFormat,
    NotRelocatable,
    UnsupportedRelocation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ElfClass {
    Bits32,
    Bits64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ElfHeader {
    pub class: ElfClass,
    pub machine: u16,
    pub section_offset: usize,
    pub section_size: usize,
    pub section_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Section {
    pub ty: u32,
    pub offset: usize,
    pub size: usize,
    pub link: usize,
    pub flags: u64,
    pub info: usize,
    pub align: usize,
}

impl Section {
    pub(crate) const fn is_alloc(self) -> bool {
        self.flags & SHF_ALLOC != 0
    }

    pub(crate) const fn is_alloc_content(self) -> bool {
        (self.ty == SHT_PROGBITS || self.ty == SHT_NOBITS) && self.is_alloc()
    }

    /// BSS 段（NOBITS）：无文件数据，放段时零填充。
    pub(crate) const fn is_nobits(self) -> bool {
        self.ty == SHT_NOBITS
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Symbol {
    pub name: usize,
    pub shndx: usize,
    pub value: u64,
    pub kind: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Relocation {
    pub target_section: usize,
    pub offset: usize,
    pub symbol_table: usize,
    pub symbol_index: usize,
    pub kind: u32,
    pub addend: i64,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ElfObject<'a> {
    blob: &'a [u8],
    header: ElfHeader,
    sections: Vec<Section>,
}

impl<'a> ElfObject<'a> {
    pub(crate) fn parse(blob: &'a [u8]) -> Result<Self, ElfError> {
        let header = parse_header(blob)?;
        let sections = parse_sections(blob, header)?;
        Ok(Self {
            blob,
            header,
            sections,
        })
    }

    pub(crate) const fn class(&self) -> ElfClass {
        self.header.class
    }

    pub(crate) const fn machine(&self) -> u16 {
        self.header.machine
    }

    pub(crate) fn sections(&self) -> &[Section] {
        &self.sections
    }

    pub(crate) fn section(&self, index: usize) -> Result<Section, ElfError> {
        self.sections
            .get(index)
            .copied()
            .ok_or(ElfError::UnsupportedFormat)
    }

    pub(crate) fn section_data(&self, index: usize) -> Result<&[u8], ElfError> {
        let section = self.section(index)?;
        if section.ty == SHT_NOBITS {
            return Ok(&[]);
        }
        bytes_at(self.blob, section.offset, section.size)
    }

    pub(crate) fn symbol_table_index(&self) -> Result<usize, ElfError> {
        self.sections
            .iter()
            .position(|section| section.ty == SHT_SYMTAB)
            .ok_or(ElfError::UnsupportedFormat)
    }

    pub(crate) fn symbol_count(&self, symbol_table: usize) -> Result<usize, ElfError> {
        let section = self.section(symbol_table)?;
        if section.ty != SHT_SYMTAB {
            return Err(ElfError::UnsupportedFormat);
        }
        let size = symbol_size(self.class());
        if section.size % size != 0 {
            return Err(ElfError::UnsupportedFormat);
        }
        Ok(section.size / size)
    }

    pub(crate) fn symbol(&self, symbol_table: usize, index: usize) -> Result<Symbol, ElfError> {
        let section = self.section(symbol_table)?;
        let count = self.symbol_count(symbol_table)?;
        if index >= count {
            return Err(ElfError::UnsupportedFormat);
        }
        let offset = section
            .offset
            .checked_add(
                index
                    .checked_mul(symbol_size(self.class()))
                    .ok_or(ElfError::UnsupportedFormat)?,
            )
            .ok_or(ElfError::UnsupportedFormat)?;
        let sym = bytes_at(self.blob, offset, symbol_size(self.class()))?;
        match self.class() {
            ElfClass::Bits32 => Ok(Symbol {
                name: u32_at(sym, 0)? as usize,
                value: u32_at(sym, 4)? as u64,
                kind: u8_at(sym, 12)? & 0x0f,
                shndx: u16_at(sym, 14)? as usize,
            }),
            ElfClass::Bits64 => Ok(Symbol {
                name: u32_at(sym, 0)? as usize,
                value: u64_at(sym, 8)?,
                kind: u8_at(sym, 4)? & 0x0f,
                shndx: u16_at(sym, 6)? as usize,
            }),
        }
    }

    pub(crate) fn symbol_name(
        &self,
        symbol_table: usize,
        symbol: Symbol,
    ) -> Result<&[u8], ElfError> {
        let symtab = self.section(symbol_table)?;
        let strings = self.section_data(symtab.link)?;
        let tail = strings
            .get(symbol.name..)
            .ok_or(ElfError::UnsupportedFormat)?;
        let length = tail
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(ElfError::UnsupportedFormat)?;
        Ok(&tail[..length])
    }

    pub(crate) fn relocations(&self) -> Result<Vec<Relocation>, ElfError> {
        let mut relocations = Vec::new();
        let rela_size = match self.class() {
            ElfClass::Bits32 => 12,
            ElfClass::Bits64 => 24,
        };
        for section in &self.sections {
            if section.ty == SHT_REL {
                return Err(ElfError::UnsupportedRelocation);
            }
            if section.ty != SHT_RELA {
                continue;
            }
            let target = self.section(section.info)?;
            // 只有落在已加载（SHF_ALLOC）段上的重定位才需要处理；`.debug_*`
            // 等元数据段的 RELA 直接跳过，不校验也不应用。
            if !target.is_alloc() {
                continue;
            }
            let _ = self.symbol_count(section.link)?;
            if section.size % rela_size != 0 {
                return Err(ElfError::UnsupportedFormat);
            }
            for index in 0..section.size / rela_size {
                let offset = section
                    .offset
                    .checked_add(
                        index
                            .checked_mul(rela_size)
                            .ok_or(ElfError::UnsupportedFormat)?,
                    )
                    .ok_or(ElfError::UnsupportedFormat)?;
                let rela = bytes_at(self.blob, offset, rela_size)?;
                let (r_offset, symbol_index, kind, addend) = match self.class() {
                    ElfClass::Bits32 => {
                        let info = u32_at(rela, 4)?;
                        (
                            u32_at(rela, 0)? as usize,
                            (info >> 8) as usize,
                            info & 0xff,
                            u32_at(rela, 8)? as i32 as i64,
                        )
                    }
                    ElfClass::Bits64 => {
                        let info = u64_at(rela, 8)?;
                        (
                            usize::try_from(u64_at(rela, 0)?)
                                .map_err(|_| ElfError::UnsupportedFormat)?,
                            usize::try_from(info >> 32).map_err(|_| ElfError::UnsupportedFormat)?,
                            (info & 0xffff_ffff) as u32,
                            u64_at(rela, 16)? as i64,
                        )
                    }
                };
                if r_offset > target.size {
                    return Err(ElfError::UnsupportedFormat);
                }
                relocations.push(Relocation {
                    target_section: section.info,
                    offset: r_offset,
                    symbol_table: section.link,
                    symbol_index,
                    kind,
                    addend,
                });
            }
        }
        Ok(relocations)
    }
}

fn parse_header(blob: &[u8]) -> Result<ElfHeader, ElfError> {
    if blob.len() < 4 || &blob[..4] != b"\x7fELF" {
        return Err(ElfError::BadMagic);
    }
    if blob.len() < 6 || blob[5] != 1 {
        return Err(ElfError::UnsupportedFormat);
    }
    let class = match blob[4] {
        1 => ElfClass::Bits32,
        2 => ElfClass::Bits64,
        _ => return Err(ElfError::UnsupportedFormat),
    };
    let header_size = match class {
        ElfClass::Bits32 => 52,
        ElfClass::Bits64 => 64,
    };
    if blob.len() < header_size {
        return Err(ElfError::UnsupportedFormat);
    }
    if u16_at(blob, 16)? != 1 {
        return Err(ElfError::NotRelocatable);
    }
    let (machine, section_offset, section_size, section_count) = match class {
        ElfClass::Bits32 => (
            u16_at(blob, 18)?,
            u32_at(blob, 32)? as usize,
            u16_at(blob, 46)? as usize,
            u16_at(blob, 48)? as usize,
        ),
        ElfClass::Bits64 => (
            u16_at(blob, 18)?,
            usize::try_from(u64_at(blob, 40)?).map_err(|_| ElfError::UnsupportedFormat)?,
            u16_at(blob, 58)? as usize,
            u16_at(blob, 60)? as usize,
        ),
    };
    let expected_section_size = match class {
        ElfClass::Bits32 => 40,
        ElfClass::Bits64 => 64,
    };
    if section_size != expected_section_size {
        return Err(ElfError::UnsupportedFormat);
    }
    let table_size = section_size
        .checked_mul(section_count)
        .ok_or(ElfError::UnsupportedFormat)?;
    let _ = bytes_at(blob, section_offset, table_size)?;
    Ok(ElfHeader {
        class,
        machine,
        section_offset,
        section_size,
        section_count,
    })
}

fn parse_sections(blob: &[u8], header: ElfHeader) -> Result<Vec<Section>, ElfError> {
    let mut sections = Vec::new();
    for index in 0..header.section_count {
        let offset = header
            .section_offset
            .checked_add(
                index
                    .checked_mul(header.section_size)
                    .ok_or(ElfError::UnsupportedFormat)?,
            )
            .ok_or(ElfError::UnsupportedFormat)?;
        let section_header = bytes_at(blob, offset, header.section_size)?;
        let section = match header.class {
            ElfClass::Bits32 => Section {
                ty: u32_at(section_header, 4)?,
                flags: u32_at(section_header, 8)? as u64,
                offset: u32_at(section_header, 16)? as usize,
                size: u32_at(section_header, 20)? as usize,
                link: u32_at(section_header, 24)? as usize,
                info: u32_at(section_header, 28)? as usize,
                align: u32_at(section_header, 32)? as usize,
            },
            ElfClass::Bits64 => Section {
                ty: u32_at(section_header, 4)?,
                flags: u64_at(section_header, 8)?,
                offset: usize::try_from(u64_at(section_header, 24)?)
                    .map_err(|_| ElfError::UnsupportedFormat)?,
                size: usize::try_from(u64_at(section_header, 32)?)
                    .map_err(|_| ElfError::UnsupportedFormat)?,
                link: u32_at(section_header, 40)? as usize,
                info: u32_at(section_header, 44)? as usize,
                align: usize::try_from(u64_at(section_header, 48)?)
                    .map_err(|_| ElfError::UnsupportedFormat)?,
            },
        };
        if section.ty != SHT_NOBITS {
            let _ = bytes_at(blob, section.offset, section.size)?;
        }
        sections.push(section);
    }
    Ok(sections)
}

fn symbol_size(class: ElfClass) -> usize {
    match class {
        ElfClass::Bits32 => 16,
        ElfClass::Bits64 => 24,
    }
}

fn bytes_at(blob: &[u8], offset: usize, size: usize) -> Result<&[u8], ElfError> {
    let end = offset
        .checked_add(size)
        .ok_or(ElfError::UnsupportedFormat)?;
    blob.get(offset..end).ok_or(ElfError::UnsupportedFormat)
}

fn u8_at(blob: &[u8], offset: usize) -> Result<u8, ElfError> {
    blob.get(offset).copied().ok_or(ElfError::UnsupportedFormat)
}

fn u16_at(blob: &[u8], offset: usize) -> Result<u16, ElfError> {
    Ok(u16::from_le_bytes(
        bytes_at(blob, offset, 2)?.try_into().unwrap(),
    ))
}

fn u32_at(blob: &[u8], offset: usize) -> Result<u32, ElfError> {
    Ok(u32::from_le_bytes(
        bytes_at(blob, offset, 4)?.try_into().unwrap(),
    ))
}

fn u64_at(blob: &[u8], offset: usize) -> Result<u64, ElfError> {
    Ok(u64::from_le_bytes(
        bytes_at(blob, offset, 8)?.try_into().unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    //! 直接单元测试：不经过 loader，直接喂 `ElfObject::parse`。
    //! 覆盖 ELF32/ELF64、各类损坏输入与整数边界；核心性质是
    //! "任意字节输入 → Ok 或 Err，绝不 panic"。

    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    // -- 手工构造最小 ET_REL 对象 ----------------------------------------

    const MACHINE: u16 = 0xF3;

    /// e_ident（16 字节）。`class`: 1=ELF32, 2=ELF64。
    fn ident(class: u8) -> [u8; 16] {
        let mut ident = [0u8; 16];
        ident[..4].copy_from_slice(b"\x7fELF");
        ident[4] = class;
        ident[5] = 1; // little-endian
        ident[6] = 1; // version
        ident
    }

    fn le16(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }
    fn le32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }
    fn le64(v: u64) -> [u8; 8] {
        v.to_le_bytes()
    }

    fn patch(blob: &mut [u8], offset: usize, bytes: &[u8]) {
        blob[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    /// ELF32 头（52 字节）。
    fn header32(shoff: u32, shnum: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&ident(1));
        h.extend_from_slice(&le16(1)); // e_type = ET_REL
        h.extend_from_slice(&le16(MACHINE));
        h.extend_from_slice(&le32(1)); // e_version
        h.extend_from_slice(&le32(0)); // e_entry
        h.extend_from_slice(&le32(0)); // e_phoff
        h.extend_from_slice(&le32(shoff));
        h.extend_from_slice(&le32(0)); // e_flags
        h.extend_from_slice(&le16(52)); // e_ehsize
        h.extend_from_slice(&le16(0)); // e_phentsize
        h.extend_from_slice(&le16(0)); // e_phnum
        h.extend_from_slice(&le16(40)); // e_shentsize
        h.extend_from_slice(&le16(shnum));
        h.extend_from_slice(&le16(0)); // e_shstrndx
        assert_eq!(h.len(), 52);
        h
    }

    /// ELF64 头（64 字节）。
    fn header64(shoff: u64, shnum: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&ident(2));
        h.extend_from_slice(&le16(1)); // e_type = ET_REL
        h.extend_from_slice(&le16(MACHINE));
        h.extend_from_slice(&le32(1)); // e_version
        h.extend_from_slice(&le64(0)); // e_entry
        h.extend_from_slice(&le64(0)); // e_phoff
        h.extend_from_slice(&le64(shoff));
        h.extend_from_slice(&le32(0)); // e_flags
        h.extend_from_slice(&le16(64)); // e_ehsize
        h.extend_from_slice(&le16(0)); // e_phentsize
        h.extend_from_slice(&le16(0)); // e_phnum
        h.extend_from_slice(&le16(64)); // e_shentsize
        h.extend_from_slice(&le16(shnum));
        h.extend_from_slice(&le16(0)); // e_shstrndx
        assert_eq!(h.len(), 64);
        h
    }

    fn shdr64(
        ty: u32,
        flags: u64,
        offset: u64,
        size: u64,
        link: u32,
        info: u32,
        entsize: u64,
    ) -> Vec<u8> {
        let mut s = Vec::new();
        s.extend_from_slice(&le32(0)); // sh_name
        s.extend_from_slice(&le32(ty));
        s.extend_from_slice(&le64(flags));
        s.extend_from_slice(&le64(0)); // sh_addr
        s.extend_from_slice(&le64(offset));
        s.extend_from_slice(&le64(size));
        s.extend_from_slice(&le32(link));
        s.extend_from_slice(&le32(info));
        s.extend_from_slice(&le64(4)); // sh_addralign
        s.extend_from_slice(&le64(entsize));
        assert_eq!(s.len(), 64);
        s
    }

    fn shdr32(
        ty: u32,
        flags: u32,
        offset: u32,
        size: u32,
        link: u32,
        info: u32,
        entsize: u32,
    ) -> Vec<u8> {
        let mut s = Vec::new();
        s.extend_from_slice(&le32(0)); // sh_name
        s.extend_from_slice(&le32(ty));
        s.extend_from_slice(&le32(flags));
        s.extend_from_slice(&le32(0)); // sh_addr
        s.extend_from_slice(&le32(offset));
        s.extend_from_slice(&le32(size));
        s.extend_from_slice(&le32(link));
        s.extend_from_slice(&le32(info));
        s.extend_from_slice(&le32(4)); // sh_addralign
        s.extend_from_slice(&le32(entsize));
        assert_eq!(s.len(), 40);
        s
    }

    fn sym64(name: u32, value: u64, shndx: u16, info: u8) -> Vec<u8> {
        let mut s = Vec::new();
        s.extend_from_slice(&le32(name));
        s.push(info);
        s.push(0); // st_other
        s.extend_from_slice(&le16(shndx));
        s.extend_from_slice(&le64(value));
        s.extend_from_slice(&le64(0)); // st_size
        assert_eq!(s.len(), 24);
        s
    }

    fn sym32(name: u32, value: u32, shndx: u16, info: u8) -> Vec<u8> {
        let mut s = Vec::new();
        s.extend_from_slice(&le32(name));
        s.extend_from_slice(&le32(value));
        s.extend_from_slice(&le32(0)); // st_size
        s.push(info);
        s.push(0); // st_other
        s.extend_from_slice(&le16(shndx));
        assert_eq!(s.len(), 16);
        s
    }

    fn rela64(offset: u64, symbol: u32, kind: u32, addend: i64) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(&le64(offset));
        r.extend_from_slice(&le64(((symbol as u64) << 32) | kind as u64));
        r.extend_from_slice(&le64(addend as u64));
        assert_eq!(r.len(), 24);
        r
    }

    fn rela32(offset: u32, symbol: u32, kind: u32, addend: i32) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(&le32(offset));
        r.extend_from_slice(&le32((symbol << 8) | kind));
        r.extend_from_slice(&le32(addend as u32));
        assert_eq!(r.len(), 12);
        r
    }

    /// 最小合法 ELF64：NULL + .text + .symtab + .strtab + .rela.text。
    fn build_elf64() -> Vec<u8> {
        let text = b"\x13\x00\x00\x00".to_vec(); // 4 字节 .text
        let strtab = b"\0kcomp_init\0kcore_x\0".to_vec();
        let symtab = [
            sym64(0, 0, 0, 0),     // NULL
            sym64(1, 0, 1, 0x02),  // kcomp_init: FUNC, .text
            sym64(12, 0, 0, 0x02), // kcore_x: UNDEF
        ]
        .concat();
        let rela = rela64(0, 2, 18, -4).to_vec(); // CALL kcore_x @ text+0

        let shoff = 64usize; // 头后直接放 section headers 占位
        let shnum = 5u16;
        let mut blob = header64(shoff as u64, shnum);
        // 数据段紧随 section table 之后
        let sections = [
            shdr64(0, 0, 0, 0, 0, 0, 0), // NULL
            shdr64(SHT_PROGBITS, SHF_ALLOC, 0, text.len() as u64, 0, 0, 0),
            shdr64(SHT_SYMTAB, 0, 0, symtab.len() as u64, 3, 1, 24),
            shdr64(3, 0, 0, strtab.len() as u64, 0, 0, 1), // SHT_STRTAB
            shdr64(SHT_RELA, 0, 0, rela.len() as u64, 2, 1, 24),
        ];
        blob.extend_from_slice(&sections.concat());
        let text_off = blob.len();
        blob.extend_from_slice(&text);
        let symtab_off = blob.len();
        blob.extend_from_slice(&symtab);
        let strtab_off = blob.len();
        blob.extend_from_slice(&strtab);
        let rela_off = blob.len();
        blob.extend_from_slice(&rela);
        // 回填 section 的 offset（64 位 shdr 里 sh_offset 在 +24）
        patch(&mut blob, 64 + 64 + 24, &le64(text_off as u64)); // .text offset
        patch(&mut blob, 64 + 128 + 24, &le64(symtab_off as u64)); // .symtab offset
        patch(&mut blob, 64 + 192 + 24, &le64(strtab_off as u64)); // .strtab offset
        patch(&mut blob, 64 + 256 + 24, &le64(rela_off as u64)); // .rela offset
        blob
    }

    /// 最小合法 ELF32：NULL + .text + .symtab + .strtab + .rela.text。
    fn build_elf32() -> Vec<u8> {
        let text = b"\x01\x00\x00\x00".to_vec();
        let strtab = b"\0kcomp_init\0kcore_x\0".to_vec();
        let symtab = [
            sym32(0, 0, 0, 0),
            sym32(1, 0, 1, 0x02),
            sym32(12, 0, 0, 0x02),
        ]
        .concat();
        let rela = rela32(0, 2, 18, -4).to_vec();

        let shoff = 52usize;
        let shnum = 5u16;
        let mut blob = header32(shoff as u32, shnum);
        let sections = [
            shdr32(0, 0, 0, 0, 0, 0, 0),
            shdr32(
                SHT_PROGBITS,
                SHF_ALLOC as u32,
                0,
                text.len() as u32,
                0,
                0,
                0,
            ),
            shdr32(SHT_SYMTAB, 0, 0, symtab.len() as u32, 3, 1, 16),
            shdr32(3, 0, 0, strtab.len() as u32, 0, 0, 1),
            shdr32(SHT_RELA, 0, 0, rela.len() as u32, 2, 1, 12),
        ];
        blob.extend_from_slice(&sections.concat());
        let text_off = blob.len();
        blob.extend_from_slice(&text);
        let symtab_off = blob.len();
        blob.extend_from_slice(&symtab);
        let strtab_off = blob.len();
        blob.extend_from_slice(&strtab);
        let rela_off = blob.len();
        blob.extend_from_slice(&rela);
        // 回填 section 的 offset（32 位 shdr 里 sh_offset 在 +16）
        patch(&mut blob, 52 + 40 + 16, &le32(text_off as u32));
        patch(&mut blob, 52 + 80 + 16, &le32(symtab_off as u32));
        patch(&mut blob, 52 + 120 + 16, &le32(strtab_off as u32));
        patch(&mut blob, 52 + 160 + 16, &le32(rela_off as u32));
        blob
    }

    fn leak(v: Vec<u8>) -> &'static [u8] {
        alloc::boxed::Box::leak(v.into_boxed_slice())
    }

    // -- 合法路径 -----------------------------------------------------------

    #[test]
    fn parses_valid_elf64() {
        let object = ElfObject::parse(leak(build_elf64())).expect("parse elf64");
        assert_eq!(object.class(), ElfClass::Bits64);
        assert_eq!(object.machine(), MACHINE);
        assert_eq!(object.sections().len(), 5);
        // 符号表：3 个符号（含 NULL）
        let symtab = object.symbol_table_index().unwrap();
        assert_eq!(symtab, 2);
        assert_eq!(object.symbol_count(symtab).unwrap(), 3);
        let sym = object.symbol(symtab, 1).unwrap();
        assert_eq!(object.symbol_name(symtab, sym).unwrap(), b"kcomp_init");
        assert_eq!(sym.shndx, 1);
        assert_eq!(sym.kind, 0x02);
        // 重定位：1 条 CALL 指向符号 2
        let relocations = object.relocations().unwrap();
        assert_eq!(relocations.len(), 1);
        assert_eq!(relocations[0].kind, 18);
        assert_eq!(relocations[0].symbol_index, 2);
        assert_eq!(relocations[0].offset, 0);
        assert_eq!(relocations[0].target_section, 1);
    }

    #[test]
    fn parses_valid_elf32() {
        let object = ElfObject::parse(leak(build_elf32())).expect("parse elf32");
        assert_eq!(object.class(), ElfClass::Bits32);
        assert_eq!(object.machine(), MACHINE);
        assert_eq!(object.sections().len(), 5);
        let symtab = object.symbol_table_index().unwrap();
        assert_eq!(object.symbol_count(symtab).unwrap(), 3);
        let sym = object.symbol(symtab, 1).unwrap();
        assert_eq!(object.symbol_name(symtab, sym).unwrap(), b"kcomp_init");
        let relocations = object.relocations().unwrap();
        assert_eq!(relocations.len(), 1);
        assert_eq!(relocations[0].symbol_index, 2);
    }

    #[test]
    fn section_data_reflects_file_bytes() {
        let blob = leak(build_elf64());
        let object = ElfObject::parse(blob).expect("parse");
        let data = object.section_data(1).expect("text data");
        assert_eq!(data, b"\x13\x00\x00\x00");
        assert_eq!(
            data.as_ptr() as usize - blob.as_ptr() as usize,
            object.sections()[1].offset,
            "section data 必须零拷贝引用原 blob"
        );
    }

    #[test]
    fn section_alignment_is_parsed() {
        // ELF64：.text 的 sh_addralign 位于 shdr +48
        let mut blob = build_elf64();
        patch(&mut blob, 64 + 64 + 48, &le64(8));
        let object = ElfObject::parse(leak(blob)).expect("parse elf64");
        assert_eq!(object.sections()[1].align, 8);

        // ELF32：.text 的 sh_addralign 位于 shdr +32
        let mut blob = build_elf32();
        patch(&mut blob, 52 + 40 + 32, &le32(2));
        let object = ElfObject::parse(leak(blob)).expect("parse elf32");
        assert_eq!(object.sections()[1].align, 2);
    }

    // 需要 os/core/build.rs 生成的真实 `.kcomp` fixture；KALEIDOS_CORE_ONLY 下
    // 跳过组件构建，故用 `no_kcomp` 门控（本模块其余用例两种模式都运行）。
    #[cfg(not(no_kcomp))]
    #[test]
    fn real_kcomp_parses_as_elf64() {
        let kcomp = include_bytes!(concat!(env!("OUT_DIR"), "/core_test.kcomp"));
        let object = ElfObject::parse(kcomp).expect("parse real core_test.kcomp");
        assert_eq!(object.class(), ElfClass::Bits64);
        assert!(object.sections().len() >= 4);
        assert!(
            object
                .symbol_count(object.symbol_table_index().unwrap())
                .unwrap()
                > 0
        );
    }

    // -- 非法输入：全部返回 Err，绝不 panic ---------------------------------

    #[test]
    fn bad_magic_is_rejected() {
        let mut blob = build_elf64();
        blob[..4].copy_from_slice(b"NOT!");
        assert_eq!(ElfObject::parse(leak(blob)), Err(ElfError::BadMagic));
    }

    #[test]
    fn wrong_class_is_rejected() {
        let mut blob = build_elf64();
        blob[4] = 3;
        assert_eq!(
            ElfObject::parse(leak(blob)),
            Err(ElfError::UnsupportedFormat)
        );
    }

    #[test]
    fn wrong_endian_is_rejected() {
        let mut blob = build_elf64();
        blob[5] = 2;
        assert_eq!(
            ElfObject::parse(leak(blob)),
            Err(ElfError::UnsupportedFormat)
        );
    }

    #[test]
    fn wrong_object_type_is_rejected() {
        let mut blob = build_elf64();
        patch(&mut blob, 16, &le16(2)); // ET_EXEC
        assert_eq!(ElfObject::parse(leak(blob)), Err(ElfError::NotRelocatable));
    }

    #[test]
    fn truncated_headers_are_rejected() {
        // ELF64 头 64 字节：截到 63 / 52 / 6 都必须 Err
        for len in [0usize, 4, 6, 52, 63] {
            let blob = leak(build_elf64()[..len].to_vec());
            assert!(
                ElfObject::parse(blob).is_err(),
                "truncated to {len} bytes must fail"
            );
        }
        // ELF32 头 52 字节
        let blob = leak(build_elf32()[..51].to_vec());
        assert!(ElfObject::parse(blob).is_err());
    }

    #[test]
    fn bad_section_header_size_is_rejected() {
        let mut blob = build_elf64();
        patch(&mut blob, 58, &le16(48)); // e_shentsize 错
        assert_eq!(
            ElfObject::parse(leak(blob)),
            Err(ElfError::UnsupportedFormat)
        );
    }

    #[test]
    fn section_table_outside_file_is_rejected() {
        let mut blob = build_elf64();
        patch(&mut blob, 40, &le64(0xFFFF_FFFF_FFFF_F000)); // e_shoff 巨大
        assert_eq!(
            ElfObject::parse(leak(blob)),
            Err(ElfError::UnsupportedFormat)
        );
    }

    #[test]
    fn section_header_count_overflow_is_rejected() {
        // shnum=0xFFFF × shentsize=64 在 64 位不会溢出 usize，但字节范围
        // 必然超出文件 → UnsupportedFormat（覆盖 "section table overflow" 语义）。
        let mut blob = build_elf64();
        patch(&mut blob, 60, &le16(0xFFFF));
        assert_eq!(
            ElfObject::parse(leak(blob)),
            Err(ElfError::UnsupportedFormat)
        );
    }

    #[test]
    fn malformed_symbol_table_is_rejected() {
        let mut blob = build_elf64();
        // .symtab size 改成 23（不是 24 的倍数）
        patch(&mut blob, 64 + 128 + 32, &le64(23));
        let object = ElfObject::parse(leak(blob)).expect("parse ok");
        let symtab = object.symbol_table_index().unwrap();
        assert_eq!(
            object.symbol_count(symtab),
            Err(ElfError::UnsupportedFormat)
        );
    }

    #[test]
    fn symbol_index_out_of_range_is_rejected() {
        let blob = leak(build_elf64());
        let object = ElfObject::parse(blob).expect("parse ok");
        let symtab = object.symbol_table_index().unwrap();
        assert_eq!(object.symbol(symtab, 3), Err(ElfError::UnsupportedFormat));
    }

    #[test]
    fn string_table_without_nul_is_rejected() {
        let mut blob = build_elf64();
        // .strtab 位于数据区末尾：布局 = [header][sections][text][symtab][strtab][rela]
        // 覆盖整个 strtab 为无 NUL 的字节，符号名解析必须失败。
        let strtab_start = blob.len() - 24 - 20;
        assert_eq!(
            &blob[strtab_start..strtab_start + 20],
            b"\0kcomp_init\0kcore_x\0"
        );
        blob[strtab_start..strtab_start + 20].copy_from_slice(b"ABCDEFGHIJKLMNOPQRST");
        let object = ElfObject::parse(leak(blob)).expect("parse ok");
        let symtab = object.symbol_table_index().unwrap();
        let sym = object.symbol(symtab, 1).unwrap();
        assert_eq!(
            object.symbol_name(symtab, sym),
            Err(ElfError::UnsupportedFormat)
        );
    }

    #[test]
    fn relocation_target_bad_is_rejected() {
        let mut blob = build_elf64();
        // .rela.info → 指向不存在的 section 5（sh_info 在 64 位 shdr +44）
        patch(&mut blob, 64 + 256 + 44, &le32(5));
        let object = ElfObject::parse(leak(blob)).expect("parse ok");
        assert_eq!(object.relocations(), Err(ElfError::UnsupportedFormat));
    }

    #[test]
    fn relocations_targeting_non_alloc_section_are_skipped() {
        let mut blob = build_elf64();
        // 清掉 .text（shndx 1）的 SHF_ALLOC：flags 位于 64 位 shdr +8
        patch(&mut blob, 64 + 64 + 8, &le64(0));
        let object = ElfObject::parse(leak(blob)).expect("parse ok");
        assert!(!object.sections()[1].is_alloc());
        assert!(
            object.relocations().expect("relocations parse").is_empty(),
            "target 非 ALLOC 的 RELA 必须整段跳过"
        );
    }

    #[test]
    fn relocation_offset_out_of_range_is_rejected() {
        let mut blob = build_elf64();
        // r_offset = 100 > .text.size = 4
        let rela_off = blob.len() - 24;
        patch(&mut blob, rela_off, &le64(100));
        let object = ElfObject::parse(leak(blob)).expect("parse ok");
        assert_eq!(object.relocations(), Err(ElfError::UnsupportedFormat));
    }

    #[test]
    fn rel_is_unsupported() {
        // 把最后一个 section 从 SHT_RELA 改成 SHT_REL(9)
        let mut blob = build_elf64();
        let last_shdr = 64 + 4 * 64;
        patch(&mut blob, last_shdr + 4, &le32(SHT_REL));
        let object = ElfObject::parse(leak(blob)).expect("parse ok");
        assert_eq!(object.relocations(), Err(ElfError::UnsupportedRelocation));
    }

    #[test]
    fn malformed_rela_is_rejected() {
        let mut blob = build_elf64();
        // .rela size 改成 23（不是 24 的倍数）
        patch(&mut blob, 64 + 256 + 32, &le64(23));
        let object = ElfObject::parse(leak(blob)).expect("parse ok");
        assert_eq!(object.relocations(), Err(ElfError::UnsupportedFormat));
    }

    #[test]
    fn arbitrary_mutations_never_panic() {
        // 对合法对象做确定性变异：字节翻转 + 截断，解析只允许 Ok/Err。
        let base = build_elf64();
        let mut cases: Vec<Vec<u8>> = Vec::new();
        for len in (0..base.len()).step_by(7) {
            cases.push(base[..len].to_vec());
        }
        for i in 0..base.len() {
            let mut mutated = base.clone();
            mutated[i] ^= 0xFF;
            cases.push(mutated);
        }
        for case in cases {
            let blob = leak(case);
            let _ = ElfObject::parse(blob); // 唯一要求：不 panic
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x7f, b'E', b'L', b'F'],
            vec![0xff; 300],
            vec![
                0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
            vec![
                0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        ];
        for case in cases {
            let blob = leak(case);
            let _ = ElfObject::parse(blob);
        }
    }

    // -- Property：任意字节输入永不 panic（docs/testing.md §9）----------------

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn arbitrary_blob_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let blob = leak(bytes);
            let _ = ElfObject::parse(blob);
        }
    }

    proptest! {
        /// 对合法 ELF32/ELF64 对象做随机翻转 + 截断，不允许 panic。
        #[test]
        fn mutated_valid_objects_never_panic(
            elf32 in Just(build_elf32()),
            elf64 in Just(build_elf64()),
            index in 0usize..1 << 20,
        ) {
            for base in [&elf32, &elf64] {
                let mut mutated = base.clone();
                let i = index % mutated.len();
                mutated[i] ^= 0xFF;
                let trunc = (index * 7) % (mutated.len() + 1);
                mutated.truncate(trunc);
                let blob = leak(mutated);
                let _ = ElfObject::parse(blob);
            }
        }
    }

    proptest! {
        /// 解析成功 ≠ 安全：继续把全部访问器 API 扫一遍，任何一步不得 panic
        /// （parse 之外的 relocations/symbol/symbol_name/section_data 也在 fuzz 范围）。
        #[test]
        fn parsed_object_apis_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let blob = leak(bytes);
            let Ok(object) = ElfObject::parse(blob) else {
                return Ok(());
            };
            let _ = object.class();
            let _ = object.machine();
            let _ = object.sections();
            for index in 0..object.sections().len() {
                let _ = object.section(index);
                let _ = object.section_data(index);
            }
            if let Ok(symtab) = object.symbol_table_index() {
                if let Ok(count) = object.symbol_count(symtab) {
                    for index in 0..count {
                        if let Ok(symbol) = object.symbol(symtab, index) {
                            let _ = object.symbol_name(symtab, symbol);
                        }
                    }
                }
                let _ = object.relocations();
            }
        }
    }
}
