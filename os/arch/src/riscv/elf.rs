//! RISC-V ELF ABI and relocation implementation.

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

/// RISC-V relocator state for PCREL_HI20/PCREL_LO12_I pairs.
pub struct RiscvRelocator {
    hi_cache: Vec<((usize, usize), i64)>,
}

impl RiscvRelocator {
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

impl RelocationBackend for RiscvRelocator {
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
        crate::physical_address_of(address)
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

#[cfg(test)]
mod tests {
    //! 生产 RiscvRelocator 的 host 测试（不是复制算法的 Fake）。
    //! 验证语义而不是几个 bit：patch 后重新解码指令，计算最终 target
    //! 与期望符号地址比对（docs/testing.md §10）。

    use super::*;
    use crate::component::{Relocation, RelocationBackend, WordSize};

    const BASE: usize = 0x8000_0000;

    fn rel(
        image_offset: usize,
        kind: u32,
        addend: i64,
        target_section: usize,
        section_offset: usize,
        symbol_section: usize,
        symbol_value: usize,
    ) -> Relocation {
        Relocation {
            target_section,
            section_offset,
            image_offset,
            kind,
            addend,
            symbol_section,
            symbol_value,
        }
    }

    // -- 解码 helper ---------------------------------------------------------

    fn sext(v: i64, bits: u32) -> i64 {
        let shift = 64 - bits;
        (v << shift) >> shift
    }

    fn auipc_imm20(insn: u32) -> i64 {
        ((insn >> 12) & 0xFFFFF) as i64
    }

    fn jalr_imm12(insn: u32) -> i64 {
        sext(((insn >> 20) & 0xFFF) as i64, 12)
    }

    fn lui_imm20(insn: u32) -> i64 {
        ((insn >> 12) & 0xFFFFF) as i64
    }

    fn i_type_imm12(insn: u32) -> i64 {
        sext(((insn >> 20) & 0xFFF) as i64, 12)
    }

    /// 组装一条 AUIPC（rd, imm20）
    fn auipc(rd: u32, imm20: i64) -> u32 {
        ((imm20 as u32 & 0xFFFFF) << 12) | (rd << 7) | 0x17
    }

    /// 组装一条 JALR（rd, rs1, imm12）
    fn jalr(rd: u32, rs1: u32, imm12: i64) -> u32 {
        ((imm12 as u32 & 0xFFF) << 20) | (rs1 << 15) | (rd << 7) | 0x67
    }

    fn lui(rd: u32, imm20: i64) -> u32 {
        ((imm20 as u32 & 0xFFFFF) << 12) | (rd << 7) | 0x37
    }

    fn addi(rd: u32, rs1: u32, imm12: i64) -> u32 {
        ((imm12 as u32 & 0xFFF) << 20) | (rs1 << 15) | (rd << 7) | 0x13
    }

    // -- CALL / CALL_PLT -------------------------------------------------------

    #[test]
    fn call_decodes_to_exact_symbol_address() {
        let mut image = [0u8; 8];
        image[..4].copy_from_slice(&auipc(1, 0x12345).to_le_bytes());
        image[4..].copy_from_slice(&jalr(1, 1, 0x678).to_le_bytes());
        let mut relocator = RiscvRelocator::new();
        let symbol = BASE + 0x1234_5678;

        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_CALL, 0, 1, 0, 0, 0),
                symbol,
            )
            .expect("CALL applies");

        let a = u32::from_le_bytes(image[..4].try_into().unwrap());
        let j = u32::from_le_bytes(image[4..].try_into().unwrap());
        assert_eq!(a & 0x7F, 0x17, "auipc opcode");
        assert_eq!(j & 0x7F, 0x67, "jalr opcode");
        assert_eq!((j >> 15) & 0x1F, 1, "jalr rs1 = ra");
        assert_eq!((j >> 7) & 0x1F, 1, "jalr rd = ra");

        let target = (BASE as i64) + (auipc_imm20(a) << 12) + jalr_imm12(j);
        assert_eq!(target, symbol as i64, "解码 target 必须等于符号地址");
    }

    #[test]
    fn call_plt_behaves_like_call() {
        let mut image = [0u8; 8];
        image[..4].copy_from_slice(&auipc(1, 0x12345).to_le_bytes());
        image[4..].copy_from_slice(&jalr(1, 1, 0x678).to_le_bytes());
        let mut relocator = RiscvRelocator::new();
        let symbol = BASE + 0x1234_5678;

        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_CALL_PLT, 0, 1, 0, 0, 0),
                symbol,
            )
            .expect("CALL_PLT applies");

        let a = u32::from_le_bytes(image[..4].try_into().unwrap());
        let j = u32::from_le_bytes(image[4..].try_into().unwrap());
        let target = (BASE as i64) + (auipc_imm20(a) << 12) + jalr_imm12(j);
        assert_eq!(target, symbol as i64);
    }

    #[test]
    fn call_keeps_original_rd_and_rs1_fields() {
        // 只改 imm 字段：rd/rs1/opcode 来自原始指令
        let mut image = [0u8; 8];
        image[..4].copy_from_slice(&auipc(5, 0).to_le_bytes());
        image[4..].copy_from_slice(&jalr(6, 7, 0).to_le_bytes());
        let mut relocator = RiscvRelocator::new();
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_CALL, 0, 1, 0, 0, 0),
                BASE + 0x1000,
            )
            .unwrap();
        let a = u32::from_le_bytes(image[..4].try_into().unwrap());
        let j = u32::from_le_bytes(image[4..].try_into().unwrap());
        assert_eq!((a >> 7) & 0x1F, 5, "auipc rd 保留");
        assert_eq!((j >> 15) & 0x1F, 7, "jalr rs1 保留");
        assert_eq!((j >> 7) & 0x1F, 6, "jalr rd 保留");
    }

    #[test]
    fn call_out_of_range_is_rejected() {
        let mut image = [0u8; 8];
        let mut relocator = RiscvRelocator::new();
        let far = BASE + 0x1_0000_0000; // > ±2 GiB
        assert_eq!(
            relocator.apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_CALL, 0, 1, 0, 0, 0),
                far,
            ),
            Err(RelocationError::Unsupported)
        );
    }

    // -- PCREL 对（HI20 + LO12_I）-------------------------------------------------

    #[test]
    fn pcrel_pair_decodes_to_symbol_address() {
        let mut image = [0u8; 8];
        image[..4].copy_from_slice(&auipc(1, 0).to_le_bytes());
        image[4..].copy_from_slice(&addi(1, 1, 0).to_le_bytes());
        let mut relocator = RiscvRelocator::new();
        let symbol = BASE + 0x1234_5000; // 低 12 位 < 0x800 → 无舍入歧义

        // HI20: key = (target_section=1, section_offset=0), 缓存 v
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_PCREL_HI20, 0, 1, 0, 1, 0x10),
                symbol,
            )
            .unwrap();
        // LO12_I: 按 (symbol_section=1, symbol_value=0) 命中 HI20 的
        // (target_section=1, section_offset=0) 缓存（真实链接器语义）。
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(4, R_RISCV_PCREL_LO12_I, 0, 1, 4, 1, 0),
                symbol,
            )
            .unwrap();

        let a = u32::from_le_bytes(image[..4].try_into().unwrap());
        let i = u32::from_le_bytes(image[4..].try_into().unwrap());
        assert_eq!(a & 0x7F, 0x17, "hi 处仍是 auipc");
        let target = (BASE as i64) + (auipc_imm20(a) << 12) + i_type_imm12(i);
        assert_eq!(target, symbol as i64, "PCREL 对解码 target == 符号地址");
    }

    #[test]
    fn pcrel_lo12_without_matching_hi_is_rejected() {
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        assert_eq!(
            relocator.apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_PCREL_LO12_I, 0, 1, 0, 1, 0x99), // 无匹配 hi
                0x1000,
            ),
            Err(RelocationError::Unsupported)
        );
    }

    // -- 绝对地址对（HI20 + LO12_I）-------------------------------------------------

    #[test]
    fn hi20_lo12_absolute_decodes_to_symbol() {
        let mut image = [0u8; 8];
        image[..4].copy_from_slice(&lui(1, 0).to_le_bytes());
        image[4..].copy_from_slice(&addi(1, 1, 0).to_le_bytes());
        let mut relocator = RiscvRelocator::new();
        let symbol = 0x1234_5678;

        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_HI20, 0, 1, 0, 0, 0),
                symbol,
            )
            .unwrap();
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(4, R_RISCV_LO12_I, 0, 1, 4, 0, 0),
                symbol,
            )
            .unwrap();

        let l = u32::from_le_bytes(image[..4].try_into().unwrap());
        let i = u32::from_le_bytes(image[4..].try_into().unwrap());
        assert_eq!(l & 0x7F, 0x37, "lui opcode");
        let target = (lui_imm20(l) << 12) + i_type_imm12(i);
        assert_eq!(target, symbol as i64, "LUI+ADDI 解码 target == 符号地址");
    }

    // -- 数据重定位 ------------------------------------------------------------------

    #[test]
    fn riscv32_writes_u32_only_for_32bit_width() {
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        let reloc = rel(0, R_RISCV_32, 0, 1, 0, 0, 0);
        relocator
            .apply(WordSize::Bits32, &mut image, BASE, reloc, 0x1234_5678)
            .expect("32-bit data relocation");
        assert_eq!(image, 0x1234_5678u32.to_le_bytes());
        // 64 位宽度下 R_RISCV_32 不合法
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        assert_eq!(
            relocator.apply(WordSize::Bits64, &mut image, BASE, reloc, 0x1234_5678),
            Err(RelocationError::Unsupported)
        );
    }

    #[test]
    fn riscv64_writes_u64_only_for_64bit_width() {
        let mut image = [0u8; 8];
        let mut relocator = RiscvRelocator::new();
        let reloc = rel(0, R_RISCV_64, 0, 1, 0, 0, 0);
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                reloc,
                0x1234_5678_9ABC_DEF0,
            )
            .expect("64-bit data relocation");
        assert_eq!(image, 0x1234_5678_9ABC_DEF0u64.to_le_bytes());
        let mut image = [0u8; 8];
        let mut relocator = RiscvRelocator::new();
        assert_eq!(
            relocator.apply(WordSize::Bits32, &mut image, BASE, reloc, 0x1234),
            Err(RelocationError::Unsupported)
        );
    }

    #[test]
    fn addend_is_applied() {
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        relocator
            .apply(
                WordSize::Bits32,
                &mut image,
                BASE,
                rel(0, R_RISCV_32, 0x10, 1, 0, 0, 0),
                0x1000,
            )
            .unwrap();
        assert_eq!(image, 0x1010u32.to_le_bytes(), "S + A");
    }

    // -- noop / 未知 kind / 越界 / 溢出 -----------------------------------------------

    #[test]
    fn relax_and_zero_are_noops() {
        let image = [0xABu8; 8];
        let mut relocator = RiscvRelocator::new();
        for kind in [0u32, R_RISCV_RELAX] {
            let mut image = image;
            relocator
                .apply(
                    WordSize::Bits64,
                    &mut image,
                    BASE,
                    rel(0, kind, 0, 1, 0, 0, 0),
                    0x1234,
                )
                .expect("noop must succeed");
            assert_eq!(image, [0xAB; 8], "noop 不得改动镜像");
        }
    }

    #[test]
    fn unknown_relocation_is_rejected() {
        let mut image = [0u8; 8];
        let mut relocator = RiscvRelocator::new();
        assert_eq!(
            relocator.apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, 0xFFFF, 0, 1, 0, 0, 0),
                0x1000,
            ),
            Err(RelocationError::Unsupported)
        );
    }

    #[test]
    fn out_of_bounds_read_is_rejected() {
        // CALL 在镜像末尾：auipc 可读，jalr 越界
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        assert_eq!(
            relocator.apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_CALL, 0, 1, 0, 0, 0),
                BASE + 0x1000,
            ),
            Err(RelocationError::OutOfBounds)
        );
    }

    #[test]
    fn out_of_bounds_write_is_rejected() {
        let mut image = [0u8; 2];
        let mut relocator = RiscvRelocator::new();
        assert_eq!(
            relocator.apply(
                WordSize::Bits32,
                &mut image,
                BASE,
                rel(0, R_RISCV_32, 0, 1, 0, 0, 0),
                0x1234,
            ),
            Err(RelocationError::OutOfBounds)
        );
    }

    #[test]
    fn image_base_overflow_is_rejected() {
        let mut image = [0u8; 8];
        let mut relocator = RiscvRelocator::new();
        assert_eq!(
            relocator.apply(
                WordSize::Bits64,
                &mut image,
                usize::MAX,
                rel(1, R_RISCV_CALL, 0, 1, 0, 0, 0),
                0x1000,
            ),
            Err(RelocationError::AddressOverflow)
        );
    }

    #[test]
    fn preserves_original_bits_outside_imm_fields() {
        // HI20 只覆盖 imm20 与 opcode 位：rd 与保留位必须保持
        let mut image = [0u8; 4];
        image[..4].copy_from_slice(&auipc(3, 0).to_le_bytes());
        let mut relocator = RiscvRelocator::new();
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_HI20, 0, 1, 0, 0, 0),
                0x1234_5678,
            )
            .unwrap();
        let insn = u32::from_le_bytes(image);
        assert_eq!((insn >> 7) & 0x1F, 3, "rd 保留");
        assert_eq!(insn & 0x7F, 0x37, "opcode 保留");
    }
}
