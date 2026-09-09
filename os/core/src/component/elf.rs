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
}

impl Section {
    pub(crate) const fn is_alloc_content(self) -> bool {
        self.ty == SHT_PROGBITS && self.flags & SHF_ALLOC != 0
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
