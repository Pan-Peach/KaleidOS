use crate::vm::{MappingPermission, PageAlloc, PhysicalRange, VirtualRange};
use alloc::vec;
use alloc::vec::Vec;
use bitflags::bitflags;

pub const VM_PAGE_SIZE: usize = 4096;
pub const ENTRIES: usize = 512;
pub const LEVELS: usize = 3;

const VPN_MASK: usize = 0x1ff;
const PPN_SHIFT: usize = 10;
const PPN_MASK: usize = (1 << 44) - 1;
const LEAF_FLAGS: usize = PteFlags::R.bits() | PteFlags::W.bits() | PteFlags::X.bits();

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PteFlags: usize {
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
    pub bits: usize,
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
            bits: (ppn & PPN_MASK) << PPN_SHIFT | PteFlags::V.bits(),
        }
    }

    pub const fn new_leaf_pa(pa: usize, flags: PteFlags) -> Self {
        Self::new_leaf(pa >> 12, flags)
    }

    pub const fn new_leaf(ppn: usize, flags: PteFlags) -> Self {
        Self {
            bits: (ppn & PPN_MASK) << PPN_SHIFT | flags.bits() | PteFlags::V.bits(),
        }
    }

    pub const fn is_valid(self) -> bool {
        self.bits & PteFlags::V.bits() != 0
    }

    pub const fn is_leaf(self) -> bool {
        self.bits & LEAF_FLAGS != 0
    }

    pub const fn ppn(self) -> usize {
        (self.bits >> PPN_SHIFT) & PPN_MASK
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
    (va >> (12 + level * 9)) & VPN_MASK
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

// ---------------------------------------------------------------------------
// Dynamic Sv39PageTable —— 硬件实现（长期 KernelAddressSpace 的页表后端）
// ---------------------------------------------------------------------------
//
// 参考 rCore `PageTable`：只存 `root_ppn` + 已分配帧列表 `frames`。页表页直接从
// 物理地址解引用（v1 identity 阶段 pa==va 可直接 `(ppn << 12) as *mut`）。
// `find_pte_create` 是核心，map/unmap/translate 全架在它上面。

/// 动态 Sv39 页表。页表页来自 Core 的 buddy heap（通过 `alloc_page` 函数指针），
/// 不是第二个分配器。v1 identity 阶段，物理地址可直接当虚拟地址解引用。
#[derive(Debug)]
pub struct Sv39PageTable {
    root_ppn: usize,
    /// 已分配的页表页物理地址（保活；后续销毁/回收用）。
    frames: Vec<usize>,
    /// buddy allocator 的窄接口：返回一个已归零页的物理地址。
    alloc_page: PageAlloc,
}

impl Sv39PageTable {
    /// 创建一个空页表：从 buddy 分配一个零页作为 root，并记录其物理页号。
    ///
    /// - `alloc_page()` 失败（内存耗尽）时映射为 `MapError::Exhausted`。
    /// - root 页同时压入 `frames` 保活，后续销毁时可统一回收。
    pub fn new(alloc_page: PageAlloc) -> Result<Self, MapError> {
        let page = alloc_page().map_err(|_| MapError::Exhausted)?;
        let root_ppn = page >> 12;
        let frames = vec![page];
        Ok(Self {
            root_ppn,
            frames,
            alloc_page,
        })
    }

    /// 写 satp 用的根页表物理页号。
    pub fn root_ppn(&self) -> usize {
        self.root_ppn
    }

    /// 只读取一个页表页（512 个 PTE）。v1 identity 阶段 `(ppn << 12)` 即虚拟地址。
    fn table(ppn: usize) -> &'static [Pte; ENTRIES] {
        unsafe { &*((ppn << 12) as *const [Pte; ENTRIES]) }
    }

    /// 可写取一个页表页。返回 `&'static mut` 是刻意的：让下游拿到的 `pte`
    /// 不借用 `self`，才能在持有 `pte` 的同时继续调 `self.alloc_page()` 分配下一层
    /// （rCore 用全局 `frame_alloc()` 规避同一个 borrow 问题）。
    fn table_mut(ppn: usize) -> &'static mut [Pte; ENTRIES] {
        unsafe { &mut *((ppn << 12) as *mut [Pte; ENTRIES]) }
    }

    /// 沿 `va` 逐层找表/建表，返回指向最终叶子槽位的 `&mut Pte`（rCore `find_pte_create`）。
    ///
    /// - 命中叶子（gigapage/megapage/4K）直接返回，**不拆分大页**；若已映射，
    ///   由调用方检查 `is_valid()` 决定是否报 `AlreadyMapped`。
    /// - 中间层缺失时用 `alloc_page` 新建零页并写表项；失败返回 `MapError::Exhausted`。
    /// - 返回的 `&mut` 指向 level-0 槽位，可能是无效（待写 leaf）或已映射（大页/4K）。
    pub fn find_pte_create(&mut self, va: usize) -> Result<&mut Pte, MapError> {
        let mut ppn = self.root_ppn;
        for level in (0..=2).rev() {
            let idx = vpn(va, level);
            let pte = &mut Self::table_mut(ppn)[idx];
            // 先判叶子：命中 gigapage/megapage/4K 叶子就直接交给调用方。
            // （map_range 会检查 is_valid 决定 AlreadyMapped；这里不拆大页。）
            if pte.is_leaf() {
                return Ok(pte);
            }
            // 已到叶层：这个槽位就是要写 4K leaf 的位置，无论当前是否有效
            // 都返回，让调用方决定写 leaf 还是报 AlreadyMapped。
            if level == 0 {
                return Ok(pte);
            }
            if !pte.is_valid() {
                let page = (self.alloc_page)().map_err(|_| MapError::Exhausted)?;
                self.frames.push(page);
                // page 是物理地址；new_table_pa(pa) 内部会 >>12，这里不要再移。
                *pte = Pte::new_table_pa(page);
            }
            ppn = pte.ppn();
        }
        Err(MapError::Exhausted)
    }

    /// 只读走表：返回 `va` 对应叶子的 `&Pte`（translate 等读操作用）。
    ///
    /// 当前动态后端只创建 4K leaf；若遇到已有的大页 leaf（gigapage/megapage）
    /// 会返回它，但 translate 只按 4K 计算页内偏移（huge page 的翻译不在本对象内）。
    fn find_pte(&self, va: usize) -> Option<&Pte> {
        let mut ppn = self.root_ppn;
        for level in (0..=2).rev() {
            let idx = vpn(va, level);
            let pte = &Self::table(ppn)[idx];
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

    /// 可变走表：返回 `va` 对应叶子的 `&mut Pte`（unmap 写无效用）。
    fn find_pte_mut(&mut self, va: usize) -> Option<&mut Pte> {
        let mut ppn = self.root_ppn;
        for level in (0..=2).rev() {
            let idx = vpn(va, level);
            let pte = &mut Self::table_mut(ppn)[idx];
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

    /// 把 `va[base, end)` 逐页映射到 `pa[..]`，每页写一个 4K leaf。
    ///
    /// - 校验 `va.size == pa.size`、base/size 页对齐（否则 `Unaligned`），
    ///   以及 `base+size` 不溢出（否则 `AddressOverflow`）。
    /// - 每页先 `find_pte_create` 定位槽位；若已映射返回 `AlreadyMapped`。
    /// - **原子性**：任一页失败（分配失败或 AlreadyMapped）时回滚本次已写入的叶子，
    ///   保证后端页表与 Core 的 `mappings` 列表一致（要么全有、要么全无）。
    ///   失败路径不能直接用 `?`，要用 `match ... break` 才能走到下面的回滚。
    /// - 回滚后中间空表会保留在 `frames` 里（无害；回收属后续里程碑）。
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
                // 分配失败要 break（而不是 ?），否则会跳过下面的回滚。
                Err(e) => break Err(e),
            };
            if pte.is_valid() {
                break Err(MapError::AlreadyMapped);
            }
            *pte = Pte::new_leaf_pa(p, flags);
            v += VM_PAGE_SIZE;
            p += VM_PAGE_SIZE;
        };

        // 失败时回滚本次已写入的叶子，保证 Core 的 mappings 与 arch 页表一致。
        if result.is_err() {
            let _ = self.unmap_range(VirtualRange {
                base: start,
                size: v - start,
            });
        }
        result
    }

    /// 解除 `va[base, end)` 的映射：逐页走 `find_pte_mut`，命中叶子则清为无效。
    ///
    /// 只清叶子，不回收中间表；不分配内存，因此不会失败。
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

    /// 翻译 `va` -> 物理地址（叶子 PA | 页内偏移）；未映射返回 `None`。
    ///
    /// 当前动态后端只创建 4K leaf，故仅按 4K 计算页内偏移。
    pub fn translate(&self, va: usize) -> Option<usize> {
        if let Some(pte) = self.find_pte(va)
            && pte.is_valid()
            && pte.is_leaf()
        {
            let pa = pte.pa();
            let offset = va & (VM_PAGE_SIZE - 1);
            return Some(pa | offset);
        }
        None
    }
}

/// `MappingPermission` -> Sv39 `PteFlags`，用可失败的 `TryFrom`。
///
/// 校验（RISC-V 硬件约束）：
/// - 必须至少有一个 R/W/X 位，否则会造出"V 但非叶、PPN 指向数据页"的假表项；
///   Sv39 判定 leaf 的规则是 `R|W|X` 至少 1 位。
/// - `W` 必须搭配 `R`（`R=0 W=1` 在 RISC-V 里是保留/非法组合）。
///
/// A|D 恒置位（访问/脏位），V 由 `Pte::new_leaf` 补上。
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
    //! Sv39 纯逻辑 host 测试：PTE/VPN/permission 编码 + 动态页表
    //! map/translate/unmap + mid-map 分配失败回滚。
    //! 页面 backing 来自 mmap 页池（见 super::test_pool），生产结构零改动。

    use super::*;

    fn rw() -> MappingPermission {
        MappingPermission::READ | MappingPermission::WRITE
    }

    fn rx() -> MappingPermission {
        MappingPermission::READ | MappingPermission::EXECUTE
    }

    // -- PTE 编码 ------------------------------------------------------------

    #[test]
    fn pte_invalid_is_not_valid_not_leaf() {
        let pte = Pte::invalid();
        assert!(!pte.is_valid());
        assert!(!pte.is_leaf());
        assert_eq!(pte.bits, 0);
    }

    #[test]
    fn pte_table_ppn_roundtrip() {
        for ppn in [0usize, 1, 0x12345, (1 << 44) - 1] {
            let pte = Pte::new_table(ppn);
            assert!(pte.is_valid(), "table entry must be valid");
            assert!(!pte.is_leaf(), "R/W/X 全 0 → 非叶");
            assert_eq!(pte.ppn(), ppn & PPN_MASK);
        }
    }

    #[test]
    fn pte_leaf_pa_roundtrip() {
        for pa in [0usize, VM_PAGE_SIZE, 0x8000_0000, 0x1_0000_0000] {
            let pte = Pte::new_leaf_pa(pa, PteFlags::R);
            assert!(pte.is_valid());
            assert!(pte.is_leaf());
            assert_eq!(pte.pa(), pa, "leaf PA 必须完整往返（44 位 PPN）");
            assert!(pte.flags().contains(PteFlags::R));
        }
    }

    #[test]
    fn pte_high_ppn_truncation_is_documented() {
        // 超过 44 位的 PPN 被截断（硬件字段宽度）；编码行为必须稳定可断言。
        let pte = Pte::new_leaf(1 << 44, PteFlags::R);
        assert_eq!(pte.ppn(), 0);
        let pte = Pte::new_leaf((1 << 44) | 0xABCD, PteFlags::R);
        assert_eq!(pte.ppn(), 0xABCD);
    }

    #[test]
    fn pte_flags_roundtrip() {
        let pte = Pte::new_leaf_pa(
            0x1000,
            PteFlags::R | PteFlags::W | PteFlags::X | PteFlags::U,
        );
        assert_eq!(
            pte.flags(),
            PteFlags::R | PteFlags::W | PteFlags::X | PteFlags::U | PteFlags::V
        );
    }

    // -- VPN 计算 --------------------------------------------------------------

    #[test]
    fn vpn_level_indices() {
        // 0x0000_0000_4000_0000 → level2 index 1（每级 512 × 1GiB 跨度）
        assert_eq!(vpn(0x0000_0000_4000_0000, 2), 1);
        // 2 MiB 边界 → level1 index 1
        assert_eq!(vpn(0x0000_0000_0020_0000, 1), 1);
        // 4 KiB 边界 → level0 index 1
        assert_eq!(vpn(0x1000, 0), 1);
        // 页内偏移不参与
        assert_eq!(vpn(0x1000, 0), vpn(0x1fff, 0));
    }

    #[test]
    fn vpn_max_canonical_within_39_bits() {
        // Sv39 有效 VA 是 39 位：bit 38 决定高/低半区，VPN2 是 bits 30..38
        assert_eq!(vpn(0xFFFF_FFFF_C000_0000, 2), 511); // 高半区顶部（bit38=1）
        assert_eq!(vpn(0x0000_003F_FFFF_FFFF, 2), 0xFF); // 低半区顶部（2^38-1，bit38=0）
        assert_eq!(vpn(0x0000_0040_0000_0000, 2), 0x100); // 第一个 bit38=1 的地址
    }

    #[test]
    fn page_offset_is_low_12_bits() {
        for va in [0usize, 0xFFF, 0x1000, 0x1234] {
            assert_eq!(va & (VM_PAGE_SIZE - 1), va & 0xFFF);
        }
    }

    // -- Permission 编码 ---------------------------------------------------------

    #[test]
    fn permission_encodes_read_write_execute_user() {
        for (perm, want_r, want_w, want_x, want_u) in [
            (MappingPermission::READ, true, false, false, false),
            (rx(), true, false, true, false),
            (rw(), true, true, false, false),
            (
                MappingPermission::READ
                    | MappingPermission::WRITE
                    | MappingPermission::EXECUTE
                    | MappingPermission::USER,
                true,
                true,
                true,
                true,
            ),
        ] {
            let flags = PteFlags::try_from(perm).expect("valid permission");
            assert_eq!(flags.contains(PteFlags::R), want_r);
            assert_eq!(flags.contains(PteFlags::W), want_w);
            assert_eq!(flags.contains(PteFlags::X), want_x);
            assert_eq!(flags.contains(PteFlags::U), want_u);
        }
    }

    #[test]
    fn permission_empty_is_rejected() {
        assert_eq!(
            PteFlags::try_from(MappingPermission::empty()),
            Err(MapError::InvalidPermission)
        );
    }

    #[test]
    fn permission_write_without_read_is_rejected() {
        assert_eq!(
            PteFlags::try_from(MappingPermission::WRITE),
            Err(MapError::InvalidPermission)
        );
    }

    // -- 动态页表 walk（mmap 页池 backing）----------------------------------------

    fn table() -> Sv39PageTable {
        Sv39PageTable::new(super::super::test_pool::alloc).expect("root alloc")
    }

    fn vr(base: usize, size: usize) -> VirtualRange {
        VirtualRange { base, size }
    }
    fn pr(base: usize, size: usize) -> PhysicalRange {
        PhysicalRange { base, size }
    }

    #[test]
    fn map_single_page_translate_unmap() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init(0, 64, false);
        let mut table = table();
        let va = 0x1000usize;
        let pa = 0x9000_0000usize;

        assert!(
            table
                .map_range(vr(va, VM_PAGE_SIZE), pr(pa, VM_PAGE_SIZE), rw())
                .is_ok()
        );
        assert_eq!(table.translate(va), Some(pa));
        assert_eq!(
            table.translate(va + 0x500),
            Some(pa + 0x500),
            "页内偏移透传"
        );
        assert_eq!(table.translate(va - 1), None);
        assert_eq!(table.translate(va + VM_PAGE_SIZE), None);

        assert!(table.unmap_range(vr(va, VM_PAGE_SIZE)).is_ok());
        assert_eq!(table.translate(va), None);
    }

    #[test]
    fn map_multipage_all_translate() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init(0, 64, false);
        let mut table = table();
        let base = 0x2000_0000usize;
        let pa = 0x8000_0000usize;
        let size = 3 * VM_PAGE_SIZE;

        assert!(table.map_range(vr(base, size), pr(pa, size), rx()).is_ok());
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

    #[test]
    fn duplicate_map_is_rejected_and_keeps_original() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init(0, 64, false);
        let mut table = table();
        let va = 0x1000usize;
        assert!(
            table
                .map_range(vr(va, VM_PAGE_SIZE), pr(0x9000_0000, VM_PAGE_SIZE), rw())
                .is_ok()
        );

        // 重叠 VA 的第二次映射：AlreadyMapped，且原映射不受影响
        assert_eq!(
            table.map_range(vr(va, VM_PAGE_SIZE), pr(0xA000_0000, VM_PAGE_SIZE), rw()),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(table.translate(va), Some(0x9000_0000), "原映射必须保留");
    }

    #[test]
    fn mid_map_alloc_failure_rolls_back_written_leaves() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init(0, 256, false);
        // 范围跨两个 2MiB 边界 → 需要 root + vpn1 + 三个 vpn0 表 = 5 次分配。
        // 令第 4 次分配之后失败：前两个 vpn0 表已各写入 512 个 leaf，
        // 第三个 vpn0 表分配失败 → 本次已写入的 1024 个 leaf 必须全部回滚。
        super::super::test_pool::set_fail_after(4);
        let mut table = table();
        let base = 0x0000_0000_4000_0000usize; // 1GiB 边界（vpn2 索引干净）
        let size = VM_PAGE_SIZE * 512 * 2 + VM_PAGE_SIZE; // 4 MiB + 4 KiB
        let result = table.map_range(vr(base, size), pr(0x8000_0000, size), rw());
        assert_eq!(result, Err(MapError::Exhausted));
        // 本次已写入的 leaf 全部回滚（Core truth 与 backend 一致）
        for i in 0..1024 {
            assert_eq!(
                table.translate(base + i * VM_PAGE_SIZE),
                None,
                "leaf {i} 必须回滚"
            );
        }
        // 恢复分配能力后整段可以重新映射成功
        super::super::test_pool::set_fail_after(usize::MAX);
        assert!(
            table
                .map_range(vr(base, size), pr(0x8000_0000, size), rw())
                .is_ok()
        );
        assert_eq!(table.translate(base), Some(0x8000_0000));
        // 回滚没有破坏后续映射：后半段也可翻译
        assert_eq!(
            table.translate(base + size - VM_PAGE_SIZE),
            Some(0x8000_0000 + size - VM_PAGE_SIZE)
        );
    }

    #[test]
    fn allocator_exhaustion_at_new_is_exhausted() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init(0, 1, false);
        super::super::test_pool::set_fail_after(0);
        assert!(matches!(
            Sv39PageTable::new(super::super::test_pool::alloc),
            Err(MapError::Exhausted)
        ));
    }

    #[test]
    fn invalid_arguments_are_rejected_before_touching_tables() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init(0, 64, false);
        let mut table = table();

        // 长度不匹配
        assert_eq!(
            table.map_range(vr(0x1000, VM_PAGE_SIZE), pr(0x2000, 2 * VM_PAGE_SIZE), rw()),
            Err(MapError::Unaligned)
        );
        // VA 未对齐
        assert_eq!(
            table.map_range(vr(0x1001, VM_PAGE_SIZE), pr(0x2000, VM_PAGE_SIZE), rw()),
            Err(MapError::Unaligned)
        );
        // PA 未对齐
        assert_eq!(
            table.map_range(vr(0x1000, VM_PAGE_SIZE), pr(0x2001, VM_PAGE_SIZE), rw()),
            Err(MapError::Unaligned)
        );
        // size 未对齐
        assert_eq!(
            table.map_range(
                vr(0x1000, VM_PAGE_SIZE + 1),
                pr(0x2000, VM_PAGE_SIZE + 1),
                rw()
            ),
            Err(MapError::Unaligned)
        );
        // 地址溢出
        assert_eq!(
            table.map_range(
                vr(0xFFFF_FFFF_FFFF_F000, 0x2000),
                pr(0x8000_0000, 0x2000),
                rw()
            ),
            Err(MapError::AddressOverflow)
        );
        // 空权限
        assert_eq!(
            table.map_range(
                vr(0x1000, VM_PAGE_SIZE),
                pr(0x2000, VM_PAGE_SIZE),
                MappingPermission::empty()
            ),
            Err(MapError::InvalidPermission)
        );
        // W without R
        assert_eq!(
            table.map_range(
                vr(0x1000, VM_PAGE_SIZE),
                pr(0x2000, VM_PAGE_SIZE),
                MappingPermission::WRITE
            ),
            Err(MapError::InvalidPermission)
        );
        // 全程未写任何 leaf
        assert_eq!(table.translate(0x1000), None);
        assert_eq!(table.translate(0x1001), None);
    }

    #[test]
    fn unmap_unknown_range_is_noop() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init(0, 64, false);
        let mut table = table();
        assert!(table.unmap_range(vr(0x5000, VM_PAGE_SIZE)).is_ok());
        assert_eq!(table.translate(0x5000), None);
    }

    #[test]
    fn map_after_unmap_reuses_va() {
        let _guard = super::super::test_pool::guard();
        super::super::test_pool::init(0, 64, false);
        let mut table = table();
        let va = 0x1000usize;
        assert!(
            table
                .map_range(vr(va, VM_PAGE_SIZE), pr(0x9000_0000, VM_PAGE_SIZE), rw())
                .is_ok()
        );
        table.unmap_range(vr(va, VM_PAGE_SIZE)).unwrap();
        assert!(
            table
                .map_range(vr(va, VM_PAGE_SIZE), pr(0xA000_0000, VM_PAGE_SIZE), rw())
                .is_ok()
        );
        assert_eq!(table.translate(va), Some(0xA000_0000));
    }
}
