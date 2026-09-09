//! Host component-object backend.
//!
//! Host tests load the same RISC-V `.kcomp` objects as the QEMU profiles, so
//! the fake backend owns a host-side implementation of that component ABI.

use crate::component::{Relocation, RelocationBackend, RelocationError, WordSize};
use alloc::vec::Vec;

const R_RISCV_32: u32 = 1;
const R_RISCV_64: u32 = 2;
const R_RISCV_CALL: u32 = 18;
const R_RISCV_CALL_PLT: u32 = 19;
const R_RISCV_PCREL_HI20: u32 = 23;
const R_RISCV_PCREL_LO12_I: u32 = 24;
const R_RISCV_HI20: u32 = 26;
const R_RISCV_LO12_I: u32 = 27;
const R_RISCV_RELAX: u32 = 51;

pub struct FakeRelocator {
    hi_cache: Vec<((usize, usize), i64)>,
}

impl FakeRelocator {
    fn read_u32(image: &[u8], offset: usize) -> Result<u32, RelocationError> {
        let end = offset.checked_add(4).ok_or(RelocationError::OutOfBounds)?;
        let bytes = image.get(offset..end).ok_or(RelocationError::OutOfBounds)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn write_u32(image: &mut [u8], offset: usize, value: u32) -> Result<(), RelocationError> {
        let end = offset.checked_add(4).ok_or(RelocationError::OutOfBounds)?;
        let bytes = image
            .get_mut(offset..end)
            .ok_or(RelocationError::OutOfBounds)?;
        bytes.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn write_u64(image: &mut [u8], offset: usize, value: u64) -> Result<(), RelocationError> {
        let end = offset.checked_add(8).ok_or(RelocationError::OutOfBounds)?;
        let bytes = image
            .get_mut(offset..end)
            .ok_or(RelocationError::OutOfBounds)?;
        bytes.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }
}

impl RelocationBackend for FakeRelocator {
    const ELF_MACHINE: u16 = 0xF3;

    fn new() -> Self {
        Self {
            hi_cache: Vec::new(),
        }
    }

    fn is_noop(kind: u32) -> bool {
        kind == 0 || kind == R_RISCV_RELAX
    }

    fn normalize_symbol_address(address: usize) -> usize {
        address
    }

    fn apply(
        &mut self,
        width: WordSize,
        image: &mut [u8],
        image_base: usize,
        relocation: Relocation,
        symbol_address: usize,
    ) -> Result<(), RelocationError> {
        if Self::is_noop(relocation.kind) {
            return Ok(());
        }

        let offset = relocation.image_offset;
        let loc = image_base
            .checked_add(offset)
            .ok_or(RelocationError::AddressOverflow)?;
        let s_addr = symbol_address as i64;

        match relocation.kind {
            R_RISCV_CALL | R_RISCV_CALL_PLT => {
                let v = s_addr + relocation.addend - loc as i64;
                if !(-(1 << 31)..(1 << 31)).contains(&v) {
                    return Err(RelocationError::Unsupported);
                }
                let imm20 = ((v + 0x800) >> 12) & 0xFFFFF;
                let imm12 = (v & 0xFFF) as u32;
                let auipc = Self::read_u32(image, offset)?;
                let auipc = (imm20 as u32) << 12 | (auipc & 0xF80) | 0x17;
                let jalr_offset = offset.checked_add(4).ok_or(RelocationError::OutOfBounds)?;
                let jalr = Self::read_u32(image, jalr_offset)?;
                let jalr = (imm12 << 20) | (jalr & 0x000F_FFFF);
                Self::write_u32(image, offset, auipc)?;
                Self::write_u32(image, jalr_offset, jalr)?;
            }
            R_RISCV_PCREL_HI20 => {
                let v = s_addr + relocation.addend - loc as i64;
                self.hi_cache
                    .push(((relocation.target_section, relocation.section_offset), v));
                let imm20 = ((v + 0x800) >> 12) & 0xFFFFF;
                let auipc = Self::read_u32(image, offset)?;
                let auipc = (imm20 as u32) << 12 | (auipc & 0xF80) | 0x17;
                Self::write_u32(image, offset, auipc)?;
            }
            R_RISCV_HI20 => {
                let v = s_addr + relocation.addend;
                let imm20 = ((v + 0x800) >> 12) & 0xFFFFF;
                let lui = Self::read_u32(image, offset)?;
                let lui = (imm20 as u32) << 12 | (lui & 0xF80) | 0x37;
                Self::write_u32(image, offset, lui)?;
            }
            R_RISCV_PCREL_LO12_I => {
                let v_hi = self
                    .hi_cache
                    .iter()
                    .find(|&(key, _)| *key == (relocation.symbol_section, relocation.symbol_value))
                    .map(|(_, value)| *value)
                    .ok_or(RelocationError::Unsupported)?;
                let imm12 = (v_hi & 0xFFF) as u32;
                let insn = Self::read_u32(image, offset)?;
                let insn = (imm12 << 20) | (insn & 0x000F_FFFF);
                Self::write_u32(image, offset, insn)?;
            }
            R_RISCV_LO12_I => {
                let imm12 = ((s_addr + relocation.addend) & 0xFFF) as u32;
                let insn = Self::read_u32(image, offset)?;
                let insn = (imm12 << 20) | (insn & 0x000F_FFFF);
                Self::write_u32(image, offset, insn)?;
            }
            R_RISCV_32 => {
                if !matches!(width, WordSize::Bits32) {
                    return Err(RelocationError::Unsupported);
                }
                Self::write_u32(image, offset, (s_addr + relocation.addend) as u32)?;
            }
            R_RISCV_64 => {
                if !matches!(width, WordSize::Bits64) {
                    return Err(RelocationError::Unsupported);
                }
                Self::write_u64(image, offset, (s_addr + relocation.addend) as u64)?;
            }
            _ => return Err(RelocationError::Unsupported),
        }
        Ok(())
    }
}
