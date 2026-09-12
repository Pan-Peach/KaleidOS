//! RISC-V ELF ABI and relocation implementation.

use crate::component::{Relocation, RelocationBackend, RelocationError, WordSize};
use alloc::vec::Vec;

const R_RISCV_32: u32 = 1;
const R_RISCV_64: u32 = 2;
const R_RISCV_CALL: u32 = 18;
const R_RISCV_CALL_PLT: u32 = 19;
const R_RISCV_32_PCREL: u32 = 20;
const R_RISCV_PCREL_HI20: u32 = 23;
const R_RISCV_PCREL_LO12_I: u32 = 24;
const R_RISCV_PCREL_LO12_S: u32 = 25;
const R_RISCV_HI20: u32 = 26;
const R_RISCV_LO12_I: u32 = 27;
const R_RISCV_LO12_S: u32 = 28;
const R_RISCV_RELAX: u32 = 51;

/// RISC-V relocator state for PCREL_HI20/PCREL_LO12_I pairs.
pub struct RiscvRelocator {
    hi_cache: Vec<((usize, usize), i64)>,
}

impl RiscvRelocator {
    /// 写入必须同时落在目标 section 边界内（`image` 只保证整镜像不越界）。
    /// 这样越界重定位不会悄悄改写相邻已放置段，而是显式 `OutOfBounds`。
    fn check_section(
        relocation: &Relocation,
        offset: usize,
        width: usize,
    ) -> Result<(), RelocationError> {
        let relative = offset
            .checked_sub(relocation.image_offset)
            .and_then(|delta| relocation.section_offset.checked_add(delta))
            .ok_or(RelocationError::OutOfBounds)?;
        let end = relative
            .checked_add(width)
            .ok_or(RelocationError::OutOfBounds)?;
        if end > relocation.section_size {
            return Err(RelocationError::OutOfBounds);
        }
        Ok(())
    }

    fn read_u32(
        image: &[u8],
        offset: usize,
        relocation: &Relocation,
    ) -> Result<u32, RelocationError> {
        Self::check_section(relocation, offset, 4)?;
        let end = offset.checked_add(4).ok_or(RelocationError::OutOfBounds)?;
        let bytes = image.get(offset..end).ok_or(RelocationError::OutOfBounds)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn write_u32(
        image: &mut [u8],
        offset: usize,
        value: u32,
        relocation: &Relocation,
    ) -> Result<(), RelocationError> {
        Self::check_section(relocation, offset, 4)?;
        let end = offset.checked_add(4).ok_or(RelocationError::OutOfBounds)?;
        let bytes = image
            .get_mut(offset..end)
            .ok_or(RelocationError::OutOfBounds)?;
        bytes.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn write_u64(
        image: &mut [u8],
        offset: usize,
        value: u64,
        relocation: &Relocation,
    ) -> Result<(), RelocationError> {
        Self::check_section(relocation, offset, 8)?;
        let end = offset.checked_add(8).ok_or(RelocationError::OutOfBounds)?;
        let bytes = image
            .get_mut(offset..end)
            .ok_or(RelocationError::OutOfBounds)?;
        bytes.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    /// S + A - loc，全部 checked。中间量溢出 = 不可表示 = `Unsupported`，
    /// 绝不能 panic（恶意 addend 如 `i64::MAX` 必须安全拒绝）。
    fn checked_delta(s_addr: i64, addend: i64, loc: i64) -> Result<i64, RelocationError> {
        s_addr
            .checked_add(addend)
            .and_then(|v| v.checked_sub(loc))
            .ok_or(RelocationError::Unsupported)
    }

    /// imm20 = ((v + 0x800) >> 12) & 0xFFFFF；`v + 0x800` 也 checked。
    fn imm20(v: i64) -> Result<i64, RelocationError> {
        v.checked_add(0x800)
            .map(|x| (x >> 12) & 0xFFFFF)
            .ok_or(RelocationError::Unsupported)
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
                let v = Self::checked_delta(s_addr, relocation.addend, loc as i64)?;
                if !(-(1 << 31)..(1 << 31)).contains(&v) {
                    return Err(RelocationError::Unsupported);
                }
                let imm20 = Self::imm20(v)?;
                let imm12 = (v & 0xFFF) as u32;
                let auipc = Self::read_u32(image, offset, &relocation)?;
                let auipc = (imm20 as u32) << 12 | (auipc & 0xF80) | 0x17;
                let jalr_offset = offset.checked_add(4).ok_or(RelocationError::OutOfBounds)?;
                let jalr = Self::read_u32(image, jalr_offset, &relocation)?;
                let jalr = (imm12 << 20) | (jalr & 0x000F_FFFF);
                Self::write_u32(image, offset, auipc, &relocation)?;
                Self::write_u32(image, jalr_offset, jalr, &relocation)?;
            }
            R_RISCV_PCREL_HI20 => {
                let v = Self::checked_delta(s_addr, relocation.addend, loc as i64)?;
                // PC-relative 差值必须是 32 位有符号范围（同 CALL）。
                if !(-(1 << 31)..(1 << 31)).contains(&v) {
                    return Err(RelocationError::Unsupported);
                }
                self.hi_cache
                    .push(((relocation.target_section, relocation.section_offset), v));
                let imm20 = Self::imm20(v)?;
                let auipc = Self::read_u32(image, offset, &relocation)?;
                let auipc = (imm20 as u32) << 12 | (auipc & 0xF80) | 0x17;
                Self::write_u32(image, offset, auipc, &relocation)?;
            }
            R_RISCV_HI20 => {
                let v = s_addr
                    .checked_add(relocation.addend)
                    .ok_or(RelocationError::Unsupported)?;
                // 绝对地址必须是 32 位无符号范围（lui+addi 只能编码这么多）。
                if !(0..(1i64 << 32)).contains(&v) {
                    return Err(RelocationError::Unsupported);
                }
                let imm20 = Self::imm20(v)?;
                let lui = Self::read_u32(image, offset, &relocation)?;
                let lui = (imm20 as u32) << 12 | (lui & 0xF80) | 0x37;
                Self::write_u32(image, offset, lui, &relocation)?;
            }
            R_RISCV_PCREL_LO12_I => {
                let v_hi = self
                    .hi_cache
                    .iter()
                    .find(|&(key, _)| *key == (relocation.symbol_section, relocation.symbol_value))
                    .map(|(_, value)| *value)
                    .ok_or(RelocationError::Unsupported)?;
                let imm12 = (v_hi & 0xFFF) as u32;
                let insn = Self::read_u32(image, offset, &relocation)?;
                let insn = (imm12 << 20) | (insn & 0x000F_FFFF);
                Self::write_u32(image, offset, insn, &relocation)?;
            }
            R_RISCV_PCREL_LO12_S => {
                // S-type（load/store）：imm[11:5] → bits 31:25，imm[4:0] → bits 11:7。
                let v_hi = self
                    .hi_cache
                    .iter()
                    .find(|&(key, _)| *key == (relocation.symbol_section, relocation.symbol_value))
                    .map(|(_, value)| *value)
                    .ok_or(RelocationError::Unsupported)?;
                let imm12 = (v_hi & 0xFFF) as u32;
                let insn = Self::read_u32(image, offset, &relocation)?;
                let insn = ((imm12 & 0xFE0) << 20) | ((imm12 & 0x1F) << 7) | (insn & 0x01FF_F07F);
                Self::write_u32(image, offset, insn, &relocation)?;
            }
            R_RISCV_LO12_I => {
                let v = s_addr
                    .checked_add(relocation.addend)
                    .ok_or(RelocationError::Unsupported)?;
                // 与 HI20 配套：绝对地址必须落在 32 位无符号范围。
                if !(0..(1i64 << 32)).contains(&v) {
                    return Err(RelocationError::Unsupported);
                }
                let imm12 = (v & 0xFFF) as u32;
                let insn = Self::read_u32(image, offset, &relocation)?;
                let insn = (imm12 << 20) | (insn & 0x000F_FFFF);
                Self::write_u32(image, offset, insn, &relocation)?;
            }
            R_RISCV_LO12_S => {
                // 绝对地址的 S-type 变体（store，如 `sw`/`sd`）：
                // 与 LO12_I 同一 HI20 配套，立即数按 S-type 落位。
                let v = s_addr
                    .checked_add(relocation.addend)
                    .ok_or(RelocationError::Unsupported)?;
                if !(0..(1i64 << 32)).contains(&v) {
                    return Err(RelocationError::Unsupported);
                }
                let imm12 = (v & 0xFFF) as u32;
                let insn = Self::read_u32(image, offset, &relocation)?;
                let insn = ((imm12 & 0xFE0) << 20) | ((imm12 & 0x1F) << 7) | (insn & 0x01FF_F07F);
                Self::write_u32(image, offset, insn, &relocation)?;
            }
            R_RISCV_32 => {
                if !matches!(width, WordSize::Bits32) {
                    return Err(RelocationError::Unsupported);
                }
                // ABI = S + A (mod 2^32)：wrapping 是定义行为，绝不 panic。
                let v = s_addr.wrapping_add(relocation.addend) as u32;
                Self::write_u32(image, offset, v, &relocation)?;
            }
            R_RISCV_64 => {
                if !matches!(width, WordSize::Bits64) {
                    return Err(RelocationError::Unsupported);
                }
                // ABI = S + A (mod 2^64)。
                let v = s_addr.wrapping_add(relocation.addend) as u64;
                Self::write_u64(image, offset, v, &relocation)?;
            }
            R_RISCV_32_PCREL => {
                // ABI = S + A - P（截断为 32 位）。PC-relative 差值必须可表示为
                // 32 位有符号（同 CALL / PCREL_HI20），否则显式拒绝。
                let v = Self::checked_delta(s_addr, relocation.addend, loc as i64)?;
                if !(-(1 << 31)..(1 << 31)).contains(&v) {
                    return Err(RelocationError::Unsupported);
                }
                Self::write_u32(image, offset, v as u32, &relocation)?;
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
            section_size: usize::MAX,
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

    /// S-type（load/store）立即数解码：imm[11:5] ← bits 31:25，imm[4:0] ← bits 11:7。
    fn s_type_imm12(insn: u32) -> i64 {
        sext((((insn >> 20) & 0xFE0) | ((insn >> 7) & 0x1F)) as i64, 12)
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

    /// S-type 变体（store，如 `sw`/`sd` 的 PCREL 对，静态 mutable 访问会生成它）：
    /// 与 LO12_I 共用 hi 缓存，但立即数按 S-type 编码落位（imm[11:5]→31:25，
    /// imm[4:0]→11:7）。
    #[test]
    fn pcrel_lo12_s_decodes_to_symbol_address() {
        // sw(rs2=2, rs1=1, imm=0)：S-type 立即数全 0 的合法编码。
        let sw_zero = (2u32 << 20) | (1u32 << 15) | (0x2u32 << 12) | 0x23u32;
        let mut image = [0u8; 8];
        image[..4].copy_from_slice(&auipc(1, 0).to_le_bytes());
        image[4..].copy_from_slice(&sw_zero.to_le_bytes());
        let mut relocator = RiscvRelocator::new();
        let symbol = BASE + 0x2000_0818; // 低 12 位非零，验证 S-type 落位

        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_PCREL_HI20, 0, 1, 0, 1, 0x10),
                symbol,
            )
            .unwrap();
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(4, R_RISCV_PCREL_LO12_S, 0, 1, 4, 1, 0),
                symbol,
            )
            .unwrap();

        let a = u32::from_le_bytes(image[..4].try_into().unwrap());
        let s = u32::from_le_bytes(image[4..].try_into().unwrap());
        assert_eq!(s & 0x7F, 0x23, "S 处仍是 store");
        let target = (BASE as i64) + (auipc_imm20(a) << 12) + s_type_imm12(s);
        assert_eq!(target, symbol as i64, "PCREL S 对解码 target == 符号地址");
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

    /// 绝对地址 S-type 对（RV32 medlow 的 `sw`/`sd` 静态 mutable 访问会生成）：
    /// 与 LO12_I 同一 LUI 配套，立即数按 S-type 落位。
    #[test]
    fn hi20_lo12_s_absolute_decodes_to_symbol() {
        let sw_zero = (2u32 << 20) | (1u32 << 15) | (0x2u32 << 12) | 0x23u32;
        let mut image = [0u8; 8];
        image[..4].copy_from_slice(&lui(1, 0).to_le_bytes());
        image[4..].copy_from_slice(&sw_zero.to_le_bytes());
        let mut relocator = RiscvRelocator::new();
        let symbol = 0x1234_5818; // 低 12 位非零，验证落位

        relocator
            .apply(
                WordSize::Bits32,
                &mut image,
                BASE,
                rel(0, R_RISCV_HI20, 0, 1, 0, 0, 0),
                symbol,
            )
            .unwrap();
        relocator
            .apply(
                WordSize::Bits32,
                &mut image,
                BASE,
                rel(4, R_RISCV_LO12_S, 0, 1, 4, 0, 0),
                symbol,
            )
            .unwrap();

        let l = u32::from_le_bytes(image[..4].try_into().unwrap());
        let s = u32::from_le_bytes(image[4..].try_into().unwrap());
        assert_eq!(l & 0x7F, 0x37, "lui opcode");
        assert_eq!(s & 0x7F, 0x23, "store opcode");
        let target = (lui_imm20(l) << 12) + s_type_imm12(s);
        assert_eq!(target, symbol as i64, "LUI+SW 解码 target == 符号地址");
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

    // -- R_RISCV_32_PCREL --------------------------------------------------------

    #[test]
    fn riscv32_pcrel_encodes_s_plus_a_minus_p() {
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        // P = BASE（image_offset 0），S = BASE + 0x1234，A = 0x10 → 0x1244。
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_32_PCREL, 0x10, 1, 0, 0, 0),
                BASE + 0x1234,
            )
            .expect("32_PCREL 在 64 位镜像中合法（.eh_frame）");
        assert_eq!(image, 0x1244u32.to_le_bytes());
    }

    #[test]
    fn riscv32_pcrel_encodes_negative_delta() {
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        // S = BASE - 0x10，A = 0 → -0x10。
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_32_PCREL, 0, 1, 0, 0, 0),
                BASE - 0x10,
            )
            .expect("负 PC-relative 差值可表示");
        assert_eq!(image, (-0x10i32).to_le_bytes());
    }

    #[test]
    fn riscv32_pcrel_out_of_range_is_rejected() {
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        // Δ = 2^31 不可表示为 32 位有符号。
        assert_eq!(
            relocator.apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_32_PCREL, 0, 1, 0, 0, 0),
                BASE + (1 << 31),
            ),
            Err(RelocationError::Unsupported)
        );
    }

    // -- 目标 section 边界 -----------------------------------------------------------

    #[test]
    fn write_past_target_section_is_rejected() {
        let mut image = [0u8; 8];
        let mut relocator = RiscvRelocator::new();
        // section 只有 2 字节，4 字节写越界（即使整 image 够大）。
        let relocation = Relocation {
            section_size: 2,
            ..rel(0, R_RISCV_32, 0, 1, 0, 0, 0)
        };
        assert_eq!(
            relocator.apply(WordSize::Bits32, &mut image, BASE, relocation, 0x1234),
            Err(RelocationError::OutOfBounds)
        );
    }

    #[test]
    fn call_crossing_target_section_end_is_rejected() {
        let mut image = [0u8; 8];
        let mut relocator = RiscvRelocator::new();
        // auipc 落在 section 内，配对的 jalr（offset+4）越过 section 末尾。
        let relocation = Relocation {
            section_size: 4,
            ..rel(0, R_RISCV_CALL, 0, 1, 0, 0, 0)
        };
        assert_eq!(
            relocator.apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                relocation,
                BASE + 0x1000,
            ),
            Err(RelocationError::OutOfBounds)
        );
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

    #[test]
    fn extreme_addends_never_panic_and_overflow_is_rejected() {
        // 恶意 addend（i64::MIN/MAX）不得让中间加法溢出 panic：
        // 指令类重定位溢出 = 不可表示 = Err；数据类重定位是 mod 2^N ABI = Ok。
        let instruction_kinds: [(u32, WordSize); 6] = [
            (R_RISCV_CALL, WordSize::Bits64),
            (R_RISCV_CALL_PLT, WordSize::Bits64),
            (R_RISCV_PCREL_HI20, WordSize::Bits64),
            (R_RISCV_HI20, WordSize::Bits64),
            (R_RISCV_LO12_I, WordSize::Bits64),
            (R_RISCV_32_PCREL, WordSize::Bits64),
        ];
        for (kind, width) in instruction_kinds {
            for addend in [i64::MIN, i64::MAX] {
                let mut image = [0u8; 8];
                let mut relocator = RiscvRelocator::new();
                let result = relocator.apply(
                    width,
                    &mut image,
                    BASE,
                    rel(0, kind, addend, 1, 0, 0, 0),
                    0x1000,
                );
                assert!(
                    result.is_err(),
                    "kind {kind:#x} addend {addend}: 溢出必须拒绝而非 panic"
                );
            }
        }
        // R_RISCV_32：mod 2^32 ABI，wrapping 有定义
        let mut image = [0u8; 4];
        let mut relocator = RiscvRelocator::new();
        relocator
            .apply(
                WordSize::Bits32,
                &mut image,
                BASE,
                rel(0, R_RISCV_32, i64::MAX, 1, 0, 0, 0),
                0x1000,
            )
            .expect("R_RISCV_32 是 mod 2^32，必须成功");
        assert_eq!(image, 0x1000u32.wrapping_add(i64::MAX as u32).to_le_bytes());
        // R_RISCV_64：mod 2^64 ABI
        let mut image = [0u8; 8];
        let mut relocator = RiscvRelocator::new();
        relocator
            .apply(
                WordSize::Bits64,
                &mut image,
                BASE,
                rel(0, R_RISCV_64, i64::MIN, 1, 0, 0, 0),
                0x1234,
            )
            .expect("R_RISCV_64 是 mod 2^64，必须成功");
        assert_eq!(image, 0x1234u64.wrapping_add(i64::MIN as u64).to_le_bytes());
    }
}
