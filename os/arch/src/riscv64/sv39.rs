use crate::vm::{MappingPermission, PageAlloc, PhysicalRange, VirtualRange};
use alloc::boxed::Box;
use alloc::vec::Vec;
use bitflags::bitflags;

pub const PAGE_SIZE: usize = 4096;
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
    address & (PAGE_SIZE - 1) == 0
}

/// 低层只读 helper：在单个根页表页 `root` 上沿 `va` 找叶子 PTE。
///
/// 这是操作一个裸 `PageTable`（boot 用）的纯函数；`Sv39PageTable::find_pte`
/// 是持有 root/frames/allocator 的对象版。
pub fn find_pte(root: &mut PageTable, va: usize) -> Option<&mut Pte> {
    let mut table = &mut root.entries;

    for level in (0..=2).rev() {
        let index = vpn(va, level);
        let entry = table[index];

        if !entry.is_valid() {
            return None;
        }

        if entry.is_leaf() {
            return Some(&mut table[index]);
        }

        if level == 0 {
            return None;
        }

        table = entry.get_pte_array()?;
    }

    None
}

/// 低层 helper：在单个根页表页 `root` 上建中间表并返回叶子 PTE。
///
/// 中间页表页用全局堆 `Box` 分配（v1 identity 阶段堆地址即物理地址）。
/// 这也是 `Sv39PageTable::find_pte_create`（allocator 版）的静态对应物。
pub fn find_pte_create(root: &mut PageTable, va: usize) -> Option<&mut Pte> {
    let mut table = &mut root.entries;

    for level in (0..=2).rev() {
        let index = vpn(va, level);
        let entry = &mut table[index];

        if !entry.is_valid() {
            if level == 0 {
                return None;
            }
            let new_table = Box::into_raw(Box::new(PageTable::empty()));
            *entry = Pte::new_table_pa(new_table as usize);
        }

        if entry.is_leaf() {
            return Some(entry);
        }

        table = entry.get_pte_array()?;
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    InvalidLevel,
    Unaligned,
    AlreadyMapped,
    Exhausted,
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
        let mut frames = Vec::new();
        frames.push(page);
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

    /// 根据物理页号取到可写的页表页（512 个 PTE）。
    ///
    /// v1 identity 阶段 `pa == va`，所以 `(ppn << 12)` 直接就是该页的虚拟地址。
    /// 返回 `&'static mut` 是刻意的：让下游拿到的 `pte` 不借用 `self`，
    /// 才能在持有 `pte` 的同时继续调 `self.alloc_page()` 分配下一层
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
                *pte = Pte::new_table_pa(page >> 12);
            }
            ppn = pte.ppn();
        }
        Err(MapError::Exhausted)
    }

    /// 走表：返回 `va` 对应叶子的 `&mut Pte`（rCore `find_pte`）。缺失/非叶返回 `None`。
    ///
    /// 返回 `&mut` 是沿用 `table_mut` 的 `&'static mut` 模式（不借用 self），
    /// 供 unmap/translate 共用；这是 rCore 同款写法。
    fn find_pte(&self, va: usize) -> Option<&mut Pte> {
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
    /// - 校验 `va.size == pa.size` 且 base/size 页对齐，否则 `MapError::Unaligned`。
    /// - 每页先 `find_pte_create` 定位槽位；若已映射返回 `MapError::AlreadyMapped`。
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
        let flags = PteFlags::from(perm);
        let start = va.base;
        let end = va.base + va.size;
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
            v += PAGE_SIZE;
            p += PAGE_SIZE;
        };

        // 失败时回滚本次已写入的叶子，保证 Core 的 mappings 与 arch 页表一致。
        if result.is_err() {
            match self.unmap_range(VirtualRange {
                base: start,
                size: v - start,
            }) {
                Ok(_) => {}
                Err(e) => {
                    panic!("unmap_range failed during rollback: {:?}", e);
                }
            }
        }
        result
    }

    /// 解除 `va[base, end)` 的映射：逐页走 `find_pte`，命中叶子则清为无效。
    ///
    /// 只清叶子，不回收中间表；不分配内存，因此不会失败。
    pub fn unmap_range(&mut self, va: VirtualRange) -> Result<(), MapError> {
        let start = va.base;
        let end = va.base + va.size;
        let mut v = start;
        while v < end {
            if let Some(pte) = self.find_pte(v) {
                if pte.is_valid() && pte.is_leaf() {
                    *pte = Pte::invalid();
                }
            }
            v += PAGE_SIZE;
        }
        Ok(())
    }

    /// 翻译 `va` -> 物理地址（叶子 PA | 页内偏移）；未映射返回 `None`。
    pub fn translate(&self, va: usize) -> Option<usize> {
        if let Some(pte) = self.find_pte(va) {
            if pte.is_valid() && pte.is_leaf() {
                let pa = pte.pa();
                let offset = va & (PAGE_SIZE - 1);
                return Some(pa | offset);
            }
        }
        None
    }
}

/// `MappingPermission` -> Sv39 `PteFlags`。
///
/// R/W/X/U 在 Sv39 里占 bit1–4，而 `MappingPermission` 的 READ/WRITE/EXECUTE/USER
/// 占 bit0–3，所以左移一位 + 掩码即可对齐（V 由 `Pte::new_leaf` 补上）。
/// A|D 恒置位（访问/脏位）。
impl From<MappingPermission> for PteFlags {
    fn from(perm: MappingPermission) -> Self {
        PteFlags::from_bits_retain(((perm.bits() as usize) << 1) & 0b1_1110)
            | PteFlags::A
            | PteFlags::D
    }
}
