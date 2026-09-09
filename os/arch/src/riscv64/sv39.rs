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

#[allow(dead_code)]
impl Sv39PageTable {
    /// 用 `alloc_page` 分配一个零页作为 root。
    ///
    /// TODO(你)：
    ///   1. `let page = alloc_page().map_err(|_| MapError::Exhausted)?;`
    ///   2. `root_ppn = page >> 12;` `frames = Vec::new(); frames.push(page);`
    pub fn new(alloc_page: PageAlloc) -> Result<Self, MapError> {
        todo!("Sv39PageTable::new")
    }

    /// 写 satp 用的根页表物理页号。
    pub fn root_ppn(&self) -> usize {
        self.root_ppn
    }

    /// 物理页号 -> `&mut [Pte; ENTRIES]`。identity 阶段 `(ppn << 12) as *mut ...`。
    ///
    /// TODO(你)：`unsafe { &mut *((ppn << 12) as *mut [Pte; ENTRIES]) }`。
    /// 返回 `&'static mut` 是刻意的：让下游 `pte` 不借用 `self`，
    /// 这样才能在拿到 `pte` 后继续调 `self.alloc_page()` 分配下一层
    /// （rCore 用全局 `frame_alloc()` 规避同一个问题）。
    fn table_mut(ppn: usize) -> &'static mut [Pte; ENTRIES] {
        todo!("Sv39PageTable::table_mut")
    }

    /// 沿 `va` 逐层找/建，返回叶子 PTE —— 就是 rCore 的 `find_pte_create`。
    ///
    /// TODO(你)：照抄 rCore：
    ///   遍历 `level` = 2,1,0，`idx = vpn(va, level)`：
    ///   - `let pte = &mut Self::table_mut(ppn)[idx];`
    ///   - `level == 0`：`return Some(pte);`
    ///   - `!pte.is_valid()`：`let page = (self.alloc_page)().map_err(|_| MapError::Exhausted)?;`
    ///     `self.frames.push(page);` `*pte = Pte::new_table_pa(page >> 12);` 然后 `ppn = pte.ppn();`
    ///   - `pte.is_leaf()`：`return Some(pte);`
    ///   - 否则：`ppn = pte.ppn();`
    pub fn find_pte_create(&mut self, va: usize) -> Option<&mut Pte> {
        todo!("Sv39PageTable::find_pte_create")
    }

    /// 只读版：不建中间层，缺失/非叶返回 `None`。
    ///
    /// TODO(你)：同 `find_pte_create`，但 `!pte.is_valid()` 时直接 `return None;`。
    fn find_pte(&self, va: usize) -> Option<&mut Pte> {
        todo!("Sv39PageTable::find_pte")
    }

    /// 映射 `va[base, end)` -> `pa[..]`，逐页写叶子。
    ///
    /// TODO(你)：
    ///   1. 校验 `va.size == pa.size` 且三者页对齐，否则 `Unaligned`；
    ///   2. `let flags = to_flags(perm);`
    ///   3. 逐页：`let pte = self.find_pte_create(v).ok_or(MapError::Exhausted)?;`
    ///      若 `pte.is_valid()` 返回 `AlreadyMapped`，
    ///      否则 `*pte = Pte::new_leaf_pa(p, flags);`
    ///
    ///   注意：`find_pte_create` 返回 `Option`，而本函数返回 `Result`，
    ///   所以要用 `.ok_or(MapError::Exhausted)?`（`None` 只可能是分配失败）。
    pub fn map_range(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        perm: MappingPermission,
    ) -> Result<(), MapError> {
        todo!("Sv39PageTable::map_range")
    }

    /// 解除映射：逐页 `find_pte` 命中则清 `Pte::invalid()`。
    pub fn unmap_range(&mut self, va: VirtualRange) -> Result<(), MapError> {
        todo!("Sv39PageTable::unmap_range")
    }

    /// 翻译 `va` -> 物理地址（叶子 PA | 页内偏移）；未映射返回 `None`。
    pub fn translate(&self, va: usize) -> Option<usize> {
        todo!("Sv39PageTable::translate")
    }
}

/// Core `MappingPermission` -> Sv39 `PteFlags`。
///
/// TODO(你)：按 read/write/execute/user 置 R/W/X/U，再并上 A|D。
#[allow(dead_code)]
fn to_flags(perm: MappingPermission) -> PteFlags {
    todo!("to_flags")
}
