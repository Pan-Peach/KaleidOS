//! Sv32 page-table encoding and the dynamic translation backend.
//!
//! Sv32 is a two-level, 1024-entry translation scheme.  The backend mirrors
//! the Sv39 implementation but keeps the XLEN/format-specific constants here:
//! one 4 KiB page contains 1024 32-bit PTEs and each VPN component is 10 bits.

use crate::vm::{MappingPermission, PageAlloc, PhysicalRange, VirtualRange};
use alloc::vec::Vec;
use bitflags::bitflags;

pub const VM_PAGE_SIZE: usize = 4096;
pub const ENTRIES: usize = 1024;
pub const LEVELS: usize = 2;

const VPN_MASK: usize = 0x3ff;
const PPN_SHIFT: usize = 10;
const PPN_MASK: u32 = (1 << 22) - 1;
const LEAF_FLAGS: u32 = PteFlags::R.bits() | PteFlags::W.bits() | PteFlags::X.bits();

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PteFlags: u32 {
        const V = 1 << 0;
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
        const G = 1 << 5;
        const A = 1 << 6;
        const D = 1 << 7;
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pte {
    pub bits: u32,
}

impl Pte {
    pub const fn invalid() -> Self {
        Self { bits: 0 }
    }

    pub const fn new_table_pa(pa: usize) -> Self {
        Self::new_table(pa >> 12)
    }

    pub const fn new_table(ppn: usize) -> Self {
        Self {
            bits: ((ppn as u32) & PPN_MASK) << PPN_SHIFT | PteFlags::V.bits(),
        }
    }

    pub const fn new_leaf_pa(pa: usize, flags: PteFlags) -> Self {
        Self::new_leaf(pa >> 12, flags)
    }

    pub const fn new_leaf(ppn: usize, flags: PteFlags) -> Self {
        Self {
            bits: ((ppn as u32) & PPN_MASK) << PPN_SHIFT | flags.bits() | PteFlags::V.bits(),
        }
    }

    pub const fn is_valid(self) -> bool {
        self.bits & PteFlags::V.bits() != 0
    }

    pub const fn is_leaf(self) -> bool {
        self.bits & LEAF_FLAGS != 0
    }

    pub const fn ppn(self) -> usize {
        ((self.bits >> PPN_SHIFT) & PPN_MASK) as usize
    }

    pub const fn pa(self) -> usize {
        self.ppn() << 12
    }

    pub const fn flags(self) -> PteFlags {
        PteFlags::from_bits_retain(self.bits & 0xff)
    }

    pub const fn get_pte_array(self) -> Option<&'static mut [Pte; ENTRIES]> {
        if self.is_valid() && !self.is_leaf() {
            let table_ptr = self.pa() as *mut [Pte; ENTRIES];
            Some(unsafe { &mut *table_ptr })
        } else {
            None
        }
    }
}

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
pub struct PageTable {
    pub entries: [Pte; ENTRIES],
}

impl PageTable {
    pub const fn empty() -> Self {
        Self {
            entries: [Pte::invalid(); ENTRIES],
        }
    }
}

pub const fn vpn(va: usize, level: usize) -> usize {
    (va >> (12 + level * 10)) & VPN_MASK
}

pub const fn is_page_aligned(address: usize) -> bool {
    address & (VM_PAGE_SIZE - 1) == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    InvalidLevel,
    Unaligned,
    AlreadyMapped,
    Exhausted,
    InvalidPermission,
    AddressOverflow,
}

/// Dynamic Sv32 page table.  Page-table pages come from Core's narrow
/// allocator callback and remain retained in `frames` for the lifetime of the
/// backend, matching the current Sv39 ownership model.
#[derive(Debug)]
pub struct Sv32PageTable {
    root_ppn: usize,
    frames: Vec<usize>,
    alloc_page: PageAlloc,
}

impl Sv32PageTable {
    pub fn new(alloc_page: PageAlloc) -> Result<Self, MapError> {
        let page = alloc_page().map_err(|_| MapError::Exhausted)?;
        let root_ppn = page >> 12;
        Ok(Self {
            root_ppn,
            frames: alloc::vec![page],
            alloc_page,
        })
    }

    pub fn root_ppn(&self) -> usize {
        self.root_ppn
    }

    fn table(ppn: usize) -> &'static [Pte; ENTRIES] {
        unsafe { &*((ppn << 12) as *const [Pte; ENTRIES]) }
    }

    fn table_mut(ppn: usize) -> &'static mut [Pte; ENTRIES] {
        unsafe { &mut *((ppn << 12) as *mut [Pte; ENTRIES]) }
    }

    /// Find or create the level-0 slot for a 4 KiB mapping.
    pub fn find_pte_create(&mut self, va: usize) -> Result<&mut Pte, MapError> {
        let mut ppn = self.root_ppn;
        for level in (0..=1).rev() {
            let idx = vpn(va, level);
            let pte = &mut Self::table_mut(ppn)[idx];
            if pte.is_leaf() {
                return Ok(pte);
            }
            if level == 0 {
                return Ok(pte);
            }
            if !pte.is_valid() {
                let page = (self.alloc_page)().map_err(|_| MapError::Exhausted)?;
                self.frames.push(page);
                *pte = Pte::new_table_pa(page);
            }
            ppn = pte.ppn();
        }
        Err(MapError::Exhausted)
    }

    fn find_pte(&self, va: usize) -> Option<&Pte> {
        let mut ppn = self.root_ppn;
        for level in (0..=1).rev() {
            let pte = &Self::table(ppn)[vpn(va, level)];
            if !pte.is_valid() {
                return None;
            }
            if pte.is_leaf() {
                return Some(pte);
            }
            if level == 0 {
                return None;
            }
            ppn = pte.ppn();
        }
        None
    }

    fn find_pte_mut(&mut self, va: usize) -> Option<&mut Pte> {
        let mut ppn = self.root_ppn;
        for level in (0..=1).rev() {
            let pte = &mut Self::table_mut(ppn)[vpn(va, level)];
            if !pte.is_valid() {
                return None;
            }
            if pte.is_leaf() {
                return Some(pte);
            }
            if level == 0 {
                return None;
            }
            ppn = pte.ppn();
        }
        None
    }

    pub fn map_range(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        perm: MappingPermission,
    ) -> Result<(), MapError> {
        if va.size != pa.size
            || !is_page_aligned(va.base)
            || !is_page_aligned(pa.base)
            || !is_page_aligned(va.size)
        {
            return Err(MapError::Unaligned);
        }
        let end = va
            .base
            .checked_add(va.size)
            .ok_or(MapError::AddressOverflow)?;
        pa.base
            .checked_add(pa.size)
            .ok_or(MapError::AddressOverflow)?;
        let flags = PteFlags::try_from(perm).map_err(|_| MapError::InvalidPermission)?;
        let start = va.base;
        let mut v = start;
        let mut p = pa.base;

        let result = loop {
            if v >= end {
                break Ok(());
            }
            let pte = match self.find_pte_create(v) {
                Ok(pte) => pte,
                Err(error) => break Err(error),
            };
            if pte.is_valid() {
                break Err(MapError::AlreadyMapped);
            }
            *pte = Pte::new_leaf_pa(p, flags);
            v += VM_PAGE_SIZE;
            p += VM_PAGE_SIZE;
        };

        if result.is_err() {
            let _ = self.unmap_range(VirtualRange {
                base: start,
                size: v - start,
            });
        }
        result
    }

    pub fn unmap_range(&mut self, va: VirtualRange) -> Result<(), MapError> {
        let end = va
            .base
            .checked_add(va.size)
            .ok_or(MapError::AddressOverflow)?;
        let mut v = va.base;
        while v < end {
            if let Some(pte) = self.find_pte_mut(v)
                && pte.is_valid()
                && pte.is_leaf()
            {
                *pte = Pte::invalid();
            }
            v += VM_PAGE_SIZE;
        }
        Ok(())
    }

    pub fn translate(&self, va: usize) -> Option<usize> {
        let pte = self.find_pte(va)?;
        pte.is_valid().then(|| pte.pa() | (va & (VM_PAGE_SIZE - 1)))
    }
}

impl TryFrom<MappingPermission> for PteFlags {
    type Error = MapError;

    fn try_from(perm: MappingPermission) -> Result<Self, Self::Error> {
        if !perm.intersects(
            MappingPermission::READ | MappingPermission::WRITE | MappingPermission::EXECUTE,
        ) {
            return Err(MapError::InvalidPermission);
        }
        if perm.contains(MappingPermission::WRITE) && !perm.contains(MappingPermission::READ) {
            return Err(MapError::InvalidPermission);
        }

        let mut flags = PteFlags::A | PteFlags::D;
        if perm.contains(MappingPermission::READ) {
            flags |= PteFlags::R;
        }
        if perm.contains(MappingPermission::WRITE) {
            flags |= PteFlags::W;
        }
        if perm.contains(MappingPermission::EXECUTE) {
            flags |= PteFlags::X;
        }
        if perm.contains(MappingPermission::USER) {
            flags |= PteFlags::U;
        }
        Ok(flags)
    }
}

#[cfg(test)]
mod tests {
    //! Sv32 纯逻辑 host 测试。
    //! PTE（32 位）/ VPN（10 位 × 2 级）/ permission 在任何 host 可测；
    //! 完整动态 walk 需要"PTE 物理地址 == 真实地址 < 2^34"（22 位 PPN 不截断），
    //! 只有 Linux 能把页池 mmap 到 0x8000_0000 低地址 —— 其余平台走
    //! RV32 QEMU ArchTest 覆盖动态 walk（docs/development/testing.md §5）。

    use super::*;

    fn rw() -> MappingPermission {
        MappingPermission::READ | MappingPermission::WRITE
    }

    // -- PTE 编码（32 位宽）----------------------------------------------------

    #[test]
    fn pte_invalid_is_not_valid_not_leaf() {
        let pte = Pte::invalid();
        assert!(!pte.is_valid());
        assert!(!pte.is_leaf());
        assert_eq!(pte.bits, 0);
    }

    #[test]
    fn pte_table_ppn_roundtrip() {
        for ppn in [0usize, 1, 0x12345, (1 << 22) - 1] {
            let pte = Pte::new_table(ppn);
            assert!(pte.is_valid());
            assert!(!pte.is_leaf());
            assert_eq!(pte.ppn(), ppn & (PPN_MASK as usize));
        }
    }

    #[test]
    fn pte_leaf_pa_roundtrip_within_34_bits() {
        // 22 位 PPN → 34 位物理地址空间
        for pa in [0usize, VM_PAGE_SIZE, 0x8000_0000, 0x3_FFFF_F000] {
            let pte = Pte::new_leaf_pa(pa, PteFlags::R);
            assert!(pte.is_leaf());
            assert_eq!(pte.pa(), pa, "34 位内 leaf PA 必须完整往返");
        }
    }

    #[test]
    fn pte_pa_truncation_beyond_34_bits_is_documented() {
        // 32 位 PTE 容不下超过 2^34 的物理地址：截断行为必须稳定可断言
        let pte = Pte::new_leaf_pa(0x4_0000_0000, PteFlags::R);
        assert_eq!(pte.pa(), 0, "bit34 以上被丢弃（PPN 22 位）");
        let pte = Pte::new_leaf_pa(0x7_FFFF_F000, PteFlags::R);
        assert_eq!(pte.pa(), 0x3_FFFF_F000, "高位截断后保留低位");
    }

    #[test]
    fn pte_flags_roundtrip() {
        let pte = Pte::new_leaf_pa(0x1000, PteFlags::R | PteFlags::W | PteFlags::U);
        assert_eq!(
            pte.flags(),
            PteFlags::R | PteFlags::W | PteFlags::U | PteFlags::V
        );
    }

    // -- VPN 计算（10 位 × 2 级）--------------------------------------------------

    #[test]
    fn vpn_level_indices() {
        // Sv32: VPN1 = bits 22..31, VPN0 = bits 12..21
        assert_eq!(vpn(0x0040_0000, 1), 1); // 4 MiB 边界（VPN1 = bits 22..31）
        assert_eq!(vpn(0x0010_0000, 0), 0x100); // 1 MiB = 2^20 → VPN0 = 256
        assert_eq!(vpn(0x1000, 0), 1); // 4 KiB 边界
        assert_eq!(vpn(0x1FFF, 0), vpn(0x1000, 0), "页内偏移不参与");
    }

    #[test]
    fn vpn_full_4gib_sweep() {
        for va in [0usize, 0xFFFF_F000, 0xFFFF_FFFF] {
            assert!(vpn(va, 0) < 1024 && vpn(va, 1) < 1024);
        }
        assert_eq!(vpn(0xFFFF_FFFF, 1), 1023); // 4GiB-1 → VPN1 顶部
        assert_eq!(vpn(0xFFFF_F000, 0), 1023); // 页内偏移不影响 VPN0
    }

    // -- Permission --------------------------------------------------------------

    #[test]
    fn permission_encoding_and_rejections() {
        let flags = PteFlags::try_from(rw()).expect("RW ok");
        assert!(flags.contains(PteFlags::R));
        assert!(flags.contains(PteFlags::W));
        assert!(!flags.contains(PteFlags::X));
        assert_eq!(
            PteFlags::try_from(MappingPermission::empty()),
            Err(MapError::InvalidPermission)
        );
        assert_eq!(
            PteFlags::try_from(MappingPermission::WRITE),
            Err(MapError::InvalidPermission)
        );
    }

    // -- 动态 walk（仅 Linux：页池固定到 0x8000_0000 低地址）--------------------

    fn table() -> Sv32PageTable {
        Sv32PageTable::new(super::super::test_pool::alloc).expect("root alloc")
    }

    fn vr(base: usize, size: usize) -> VirtualRange {
        VirtualRange { base, size }
    }
    fn pr(base: usize, size: usize) -> PhysicalRange {
        PhysicalRange { base, size }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn map_single_page_translate_unmap() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init_low();
        let mut table = table();
        let va = 0x1000usize;
        let pa = 0x9000_0000usize;
        assert!(
            table
                .map_range(vr(va, VM_PAGE_SIZE), pr(pa, VM_PAGE_SIZE), rw())
                .is_ok()
        );
        assert_eq!(table.translate(va), Some(pa));
        assert_eq!(table.translate(va + 0x500), Some(pa + 0x500));
        assert_eq!(table.translate(va - 1), None);
        assert_eq!(table.translate(va + VM_PAGE_SIZE), None);
        assert!(table.unmap_range(vr(va, VM_PAGE_SIZE)).is_ok());
        assert_eq!(table.translate(va), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn map_multipage_all_translate() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init_low();
        let mut table = table();
        let base = 0x4000_0000usize;
        let pa = 0x8000_0000usize;
        let size = 3 * VM_PAGE_SIZE;
        assert!(table.map_range(vr(base, size), pr(pa, size), rw()).is_ok());
        for i in 0..3 {
            assert_eq!(
                table.translate(base + i * VM_PAGE_SIZE),
                Some(pa + i * VM_PAGE_SIZE)
            );
        }
        assert!(table.unmap_range(vr(base, size)).is_ok());
        for i in 0..3 {
            assert_eq!(table.translate(base + i * VM_PAGE_SIZE), None);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mid_map_alloc_failure_rolls_back_written_leaves() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init_low();
        // Sv32 只有两级：root 就是 VPN1 表。跨两个 4MiB 边界需要
        // root + 三个 vpn0 表 = 4 次分配；令第 4 次（第三个 vpn0 表）失败，
        // 已写入的 2048 个 leaf 必须回滚。
        super::super::test_pool::set_fail_after(3);
        let mut table = table();
        let base = 0x0040_0000usize; // 4MiB 边界（vpn1 索引干净）
        let size = VM_PAGE_SIZE * 1024 * 2 + VM_PAGE_SIZE; // 8 MiB + 4 KiB
        let result = table.map_range(vr(base, size), pr(0x8000_0000, size), rw());
        assert_eq!(result, Err(MapError::Exhausted));
        for i in 0..2048 {
            assert_eq!(
                table.translate(base + i * VM_PAGE_SIZE),
                None,
                "leaf {i} 必须回滚"
            );
        }
        super::super::test_pool::set_fail_after(usize::MAX);
        assert!(
            table
                .map_range(vr(base, size), pr(0x8000_0000, size), rw())
                .is_ok()
        );
        assert_eq!(table.translate(base), Some(0x8000_0000));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn duplicate_map_is_rejected_and_keeps_original() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init_low();
        let mut table = table();
        let va = 0x1000usize;
        assert!(
            table
                .map_range(vr(va, VM_PAGE_SIZE), pr(0x9000_0000, VM_PAGE_SIZE), rw())
                .is_ok()
        );
        assert_eq!(
            table.map_range(vr(va, VM_PAGE_SIZE), pr(0xA000_0000, VM_PAGE_SIZE), rw()),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(table.translate(va), Some(0x9000_0000), "原映射必须保留");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn invalid_arguments_are_rejected_before_touching_tables() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init_low();
        let mut table = table();
        assert_eq!(
            table.map_range(vr(0x1000, VM_PAGE_SIZE), pr(0x2000, 2 * VM_PAGE_SIZE), rw()),
            Err(MapError::Unaligned)
        );
        assert_eq!(
            table.map_range(vr(0x1001, VM_PAGE_SIZE), pr(0x2000, VM_PAGE_SIZE), rw()),
            Err(MapError::Unaligned)
        );
        assert_eq!(
            table.map_range(vr(0x1000, VM_PAGE_SIZE), pr(0x2001, VM_PAGE_SIZE), rw()),
            Err(MapError::Unaligned)
        );
        assert_eq!(
            table.map_range(
                vr(usize::MAX - 0xFFF, 0x2000),
                pr(0x8000_0000, 0x2000),
                rw()
            ),
            Err(MapError::AddressOverflow)
        );
        assert_eq!(
            table.map_range(
                vr(0x1000, VM_PAGE_SIZE),
                pr(0x2000, VM_PAGE_SIZE),
                MappingPermission::empty()
            ),
            Err(MapError::InvalidPermission)
        );
        assert_eq!(table.translate(0x1000), None);
    }
}
