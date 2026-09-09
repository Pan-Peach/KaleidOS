//! RISC-V early boot page-table plan.
//!
//! This is the small, statically backed mapping used before Core's physical
//! allocator exists.  It deliberately keeps both the identity window and the
//! high-half alias alive.  A later final-kernel mapping will replace this root
//! after memory discovery and allocation are available.

use super::sv39::{ENTRIES, PAGE_SIZE, PageTable, Pte, PteFlags, vpn};

pub const HIGH_HALF_OFFSET: usize = 0xffff_ffc0_0000_0000;
pub const KERNEL_VMA: usize = 0xffff_ffc0_8020_0000;
pub const GIGAPAGE_SIZE: usize = 1 << 30;
const MEGAPAGE_SIZE: usize = 1 << 21;

const IDENTITY_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::X)
    .union(PteFlags::A)
    .union(PteFlags::D);
const MMIO_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::A)
    .union(PteFlags::D);
/// Permission set for the executable text segment: read + execute.
pub const KERNEL_TEXT_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::X)
    .union(PteFlags::A)
    .union(PteFlags::D);
/// Permission set for read-only data (`.rodata`, embedded `.initpkg`).
pub const KERNEL_RODATA_FLAGS: PteFlags = PteFlags::R.union(PteFlags::A).union(PteFlags::D);
/// Permission set for writable data (`.data`, `.bss`).
pub const KERNEL_DATA_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::A)
    .union(PteFlags::D);

/// One contiguous linked-image run mapped with a single permission set.
///
/// `va_start`/`va_end` are high-half virtual addresses (exclusive end).  The
/// physical address of each page is derived from `linked_kernel_pa` and the
/// offset from the link-time kernel VMA, so the boot crate only needs to pass
/// the linker section ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelSection {
    pub va_start: usize,
    pub va_end: usize,
    pub flags: PteFlags,
}

/// Early root storage. It lives in the image's BSS and is never used as the
/// future per-domain AddressSpace root.
static mut BOOT_ROOT: PageTable = PageTable::empty();
static mut KERNEL_L1: PageTable = PageTable::empty();
const MAX_IDENTITY_L1: usize = 4;
static mut IDENTITY_L1S: [PageTable; MAX_IDENTITY_L1] =
    [PageTable::empty(); MAX_IDENTITY_L1];

/// Pool of level-0 tables used to map the kernel image at 4 KiB granularity.
/// The early boot path has no allocator, so it carves these from a static
/// pool; one table covers a 2 MiB level-1 slot.  16 slots span a 32 MiB
/// image, comfortably above the previous hard 2 MiB cap.
const MAX_KERNEL_L0: usize = 16;
static mut KERNEL_L0S: [PageTable; MAX_KERNEL_L0] = [PageTable::empty(); MAX_KERNEL_L0];
static mut KERNEL_L0_COUNT: usize = 0;
static mut IDENTITY_L0S: [PageTable; MAX_KERNEL_L0] = [PageTable::empty(); MAX_KERNEL_L0];
static mut IDENTITY_L0_COUNT: usize = 0;
static mut BOOT_ROOT_PA: usize = 0;

pub fn root() -> &'static mut PageTable {
    // SAFETY: boot is single-hart at this point and the root is exclusively
    // owned by the bootstrap path until the final root replaces it.
    unsafe { &mut *core::ptr::addr_of_mut!(BOOT_ROOT) }
}

pub fn root_pa() -> usize {
    // Set by `init` after converting the link-time address of BOOT_ROOT to
    // the runtime physical address of the loaded image.
    unsafe { BOOT_ROOT_PA }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootVmError {
    EmptyRam,
    AddressOverflow,
    AddressUnaligned,
    KernelOutsideRam,
    KernelMappingTooLarge,
    InvalidKernelSection,
    RootIndexConflict,
    KernelL0TableExhausted,
}

/// Install the temporary Linux-shaped boot mapping.
///
/// - root[0]      : low MMIO window
/// - root[ram_root_index] : identity-mapped RAM windows
/// - root[high_root_index] : high-half aliases of those windows
///
/// The bootstrap initially continues through the identity VA, then switches
/// to the high alias with `mmu::enter_high_half` after activation.
///
/// `ram_base` and `ram_size` come from bootstrap discovery (currently FDT).
/// The mapping is deliberately coarse: one level-2 leaf per 1 GiB window.
/// The final address space will replace this root with 4 KiB-granular tables.
pub unsafe fn init(
    kernel_pa: usize,
    linked_kernel_pa: usize,
    image_size: usize,
    ram_base: usize,
    ram_size: usize,
    sections: &[KernelSection],
) -> Result<(), BootVmError> {
    if ram_size == 0 {
        return Err(BootVmError::EmptyRam);
    }

    if kernel_pa & (PAGE_SIZE - 1) != 0
        || linked_kernel_pa & (PAGE_SIZE - 1) != 0
        || image_size == 0
    {
        return Err(BootVmError::AddressUnaligned);
    }

    let ram_end = ram_base
        .checked_add(ram_size)
        .ok_or(BootVmError::AddressOverflow)?;
    let kernel_end = kernel_pa
        .checked_add(image_size)
        .ok_or(BootVmError::AddressOverflow)?;
    if kernel_pa < ram_base || kernel_end > ram_end {
        return Err(BootVmError::KernelOutsideRam);
    }
    validate_sections(image_size, sections)?;

    let first_window = ram_base & !(GIGAPAGE_SIZE - 1);
    let last_window = ram_end
        .checked_add(GIGAPAGE_SIZE - 1)
        .ok_or(BootVmError::AddressOverflow)?
        & !(GIGAPAGE_SIZE - 1);

    let root = root();
    root.entries[0] = Pte::new_leaf_pa(0, MMIO_FLAGS);

    let mut pa = first_window;
    while pa < last_window {
        let low_index = (pa >> 30) & 0x1ff;
        let high_va = HIGH_HALF_OFFSET
            .checked_add(pa)
            .ok_or(BootVmError::AddressOverflow)?;
        let high_index = (high_va >> 30) & 0x1ff;

        // root[0] is reserved for the early MMIO aperture.  A RAM region
        // starting at physical address zero cannot use this coarse layout.
        if low_index == 0 || high_index == 0 || low_index == high_index {
            return Err(BootVmError::RootIndexConflict);
        }

        let leaf = Pte::new_leaf_pa(pa, IDENTITY_FLAGS);
        root.entries[low_index] = leaf;
        root.entries[high_index] = leaf;

        pa = pa
            .checked_add(GIGAPAGE_SIZE)
            .ok_or(BootVmError::AddressOverflow)?;
    }

    unsafe {
        install_identity_alias(root, kernel_pa, linked_kernel_pa, image_size, sections)?;
        install_kernel_alias(root, kernel_pa, linked_kernel_pa, image_size, sections)?;

        let linked_root_pa = physical_address_of(root as *const PageTable as usize);
        BOOT_ROOT_PA = runtime_physical_address(linked_root_pa, kernel_pa, linked_kernel_pa)?;
    }

    Ok(())
}

/// Replace the coarse identity leaf covering the kernel with 2 MiB leaves and
/// 4 KiB tables where the formal kernel sections need tighter permissions.
///
/// This is deliberately only a transition mapping: ordinary RAM outside the
/// kernel image remains covered by the permissive identity leaves.  Keeping
/// the kernel's low alias in sync prevents its formal text/data pages from
/// being writable merely because the bootstrap RAM map is still active.
unsafe fn install_identity_alias(
    root: &mut PageTable,
    kernel_pa: usize,
    linked_kernel_pa: usize,
    image_size: usize,
    sections: &[KernelSection],
) -> Result<(), BootVmError> {
    let image_end = kernel_pa
        .checked_add(image_size)
        .ok_or(BootVmError::AddressOverflow)?;
    let last_image_pa = image_end
        .checked_sub(1)
        .ok_or(BootVmError::AddressOverflow)?;
    let first_window = kernel_pa & !(GIGAPAGE_SIZE - 1);
    let last_window = last_image_pa & !(GIGAPAGE_SIZE - 1);
    let window_count = (last_window - first_window) / GIGAPAGE_SIZE + 1;
    if window_count > MAX_IDENTITY_L1 {
        return Err(BootVmError::KernelMappingTooLarge);
    }

    unsafe {
        IDENTITY_L0_COUNT = 0;
    }

    for window_index in 0..window_count {
        let window_pa = first_window
            .checked_add(window_index * GIGAPAGE_SIZE)
            .ok_or(BootVmError::AddressOverflow)?;
        let root_index = (window_pa >> 30) & 0x1ff;
        if root_index == 0 {
            return Err(BootVmError::RootIndexConflict);
        }

        let l1 = unsafe { &mut *core::ptr::addr_of_mut!(IDENTITY_L1S[window_index]) };
        for (index, entry) in l1.entries.iter_mut().enumerate() {
            let pa = window_pa
                .checked_add(index * MEGAPAGE_SIZE)
                .ok_or(BootVmError::AddressOverflow)?;
            *entry = Pte::new_leaf_pa(pa, IDENTITY_FLAGS);
        }

        let l1_link_pa = physical_address_of(l1 as *const PageTable as usize);
        let l1_pa = runtime_physical_address(l1_link_pa, kernel_pa, linked_kernel_pa)?;
        root.entries[root_index] = Pte::new_table_pa(l1_pa);
    }

    // Overlay the formal sections on the low identity view.  The section
    // addresses are high VMAs; convert each page through the same image
    // offset used by the high-half alias.
    for section in sections {
        if section.va_start >= section.va_end {
            continue;
        }
        let offset = section
            .va_start
            .checked_sub(KERNEL_VMA)
            .ok_or(BootVmError::AddressOverflow)?;
        let section_pa = kernel_pa
            .checked_add(offset)
            .ok_or(BootVmError::AddressOverflow)?;
        let section_end = kernel_pa
            .checked_add(
                section
                    .va_end
                    .checked_sub(KERNEL_VMA)
                    .ok_or(BootVmError::AddressOverflow)?,
            )
            .ok_or(BootVmError::AddressOverflow)?;

        let mut pa = section_pa;
        while pa < section_end {
            let window_index = (pa - first_window) / GIGAPAGE_SIZE;
            let l1 = unsafe { &mut *core::ptr::addr_of_mut!(IDENTITY_L1S[window_index]) };
            let window_pa = first_window
                .checked_add(window_index * GIGAPAGE_SIZE)
                .ok_or(BootVmError::AddressOverflow)?;
            let l0 =
                unsafe { identity_l0_for(l1, vpn(pa, 1), window_pa, kernel_pa, linked_kernel_pa)? };
            l0[vpn(pa, 0)] = Pte::new_leaf_pa(pa, section.flags);
            pa = pa
                .checked_add(PAGE_SIZE)
                .ok_or(BootVmError::AddressOverflow)?;
        }
    }

    Ok(())
}

unsafe fn install_kernel_alias(
    root: &mut PageTable,
    kernel_pa: usize,
    linked_kernel_pa: usize,
    image_size: usize,
    sections: &[KernelSection],
) -> Result<(), BootVmError> {
    // Reset the single level-1 table and release every level-0 table.
    unsafe {
        KERNEL_L0_COUNT = 0;
    }
    let l1 = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_L1) };
    for entry in l1.entries.iter_mut() {
        *entry = Pte::invalid();
    }

    let l1_link_pa = physical_address_of(core::ptr::addr_of!(KERNEL_L1) as usize);
    let l1_pa = runtime_physical_address(l1_link_pa, kernel_pa, linked_kernel_pa)?;
    root.entries[vpn(KERNEL_VMA, 2)] = Pte::new_table_pa(l1_pa);

    // Pass 1: map the entire image range (including the low bootstrap
    // trampoline's high-half shadow and any padding between sections) with a
    // writable, non-executable baseline.  The formal .text section is made
    // executable in pass 2; the bootstrap shadow only needs data access after
    // the hand-off, and must not create another RWX kernel alias.
    let mapped_size = image_size
        .checked_add(PAGE_SIZE - 1)
        .ok_or(BootVmError::AddressOverflow)?
        & !(PAGE_SIZE - 1);
    for offset in (0..mapped_size).step_by(PAGE_SIZE) {
        let va = KERNEL_VMA
            .checked_add(offset)
            .ok_or(BootVmError::AddressOverflow)?;
        let pa = kernel_pa
            .checked_add(offset)
            .ok_or(BootVmError::AddressOverflow)?;
        let l1_index = vpn(va, 1);
        let l0 = unsafe { l0_for(l1, l1_index, kernel_pa, linked_kernel_pa)? };
        l0[vpn(va, 0)] = Pte::new_leaf_pa(pa, KERNEL_DATA_FLAGS);
    }

    // Pass 2: tighten each linker section to its real permission set.  This is
    // what turns Sv39 from address relocation into actual protection.
    for section in sections {
        if section.va_start >= section.va_end {
            continue;
        }
        if section.va_start & (PAGE_SIZE - 1) != 0 {
            return Err(BootVmError::AddressUnaligned);
        }

        let mut va = section.va_start;
        while va < section.va_end {
            let l1_index = vpn(va, 1);
            let l0 = unsafe { l0_for(l1, l1_index, kernel_pa, linked_kernel_pa)? };
            let offset = va - KERNEL_VMA;
            let pa = kernel_pa
                .checked_add(offset)
                .ok_or(BootVmError::AddressOverflow)?;
            l0[vpn(va, 0)] = Pte::new_leaf_pa(pa, section.flags);
            va += PAGE_SIZE;
        }
    }

    Ok(())
}

/// Return a 4 KiB table for an identity-map 2 MiB slot, replacing its coarse
/// leaf on first use.
unsafe fn identity_l0_for(
    l1: &mut PageTable,
    l1_index: usize,
    window_pa: usize,
    kernel_pa: usize,
    linked_kernel_pa: usize,
) -> Result<&'static mut [Pte; ENTRIES], BootVmError> {
    if let Some(pte) = l1.entries[l1_index]
        .is_valid()
        .then(|| l1.entries[l1_index])
    {
        if !pte.is_leaf() {
            return pte.get_pte_array().ok_or(BootVmError::RootIndexConflict);
        }
    }

    let idx = unsafe { IDENTITY_L0_COUNT };
    if idx >= MAX_KERNEL_L0 {
        return Err(BootVmError::KernelL0TableExhausted);
    }
    unsafe {
        IDENTITY_L0_COUNT += 1;
    }

    let table = unsafe { &mut *core::ptr::addr_of_mut!(IDENTITY_L0S[idx]) };
    for entry in table.entries.iter_mut() {
        *entry = Pte::invalid();
    }
    let l0_link_pa = physical_address_of(table as *const PageTable as usize);
    let l0_pa = runtime_physical_address(l0_link_pa, kernel_pa, linked_kernel_pa)?;
    l1.entries[l1_index] = Pte::new_table_pa(l0_pa);
    // Preserve the original 2 MiB identity mapping for pages not covered by
    // a formal section.
    for (index, entry) in table.entries.iter_mut().enumerate() {
        let pa = window_pa
            .checked_add(l1_index * MEGAPAGE_SIZE)
            .and_then(|pa| pa.checked_add(index * PAGE_SIZE))
            .ok_or(BootVmError::AddressOverflow)?;
        *entry = Pte::new_leaf_pa(pa, IDENTITY_FLAGS);
    }
    Ok(&mut table.entries)
}

fn validate_sections(image_size: usize, sections: &[KernelSection]) -> Result<(), BootVmError> {
    let mapped_size = image_size
        .checked_add(PAGE_SIZE - 1)
        .ok_or(BootVmError::AddressOverflow)?
        & !(PAGE_SIZE - 1);
    let image_end = KERNEL_VMA
        .checked_add(mapped_size)
        .ok_or(BootVmError::AddressOverflow)?;

    for section in sections {
        if section.va_start == section.va_end {
            continue;
        }
        if section.va_start > section.va_end
            || section.va_start < KERNEL_VMA
            || section.va_start & (PAGE_SIZE - 1) != 0
            || section.va_end > image_end
        {
            return Err(BootVmError::InvalidKernelSection);
        }
    }
    Ok(())
}

/// Return the level-0 table for a level-1 slot, allocating one on first use.
unsafe fn l0_for(
    l1: &mut PageTable,
    l1_index: usize,
    kernel_pa: usize,
    linked_kernel_pa: usize,
) -> Result<&'static mut [Pte; ENTRIES], BootVmError> {
    match l1.entries[l1_index] {
        pte if pte.is_valid() => pte.get_pte_array().ok_or(BootVmError::RootIndexConflict),
        _ => unsafe { alloc_l0(l1, l1_index, kernel_pa, linked_kernel_pa) },
    }
}

/// Reserve a fresh level-0 table from the static pool and wire it into the
/// level-1 slot `l1_index` as a 4 KiB-granularity table.
unsafe fn alloc_l0(
    l1: &mut PageTable,
    l1_index: usize,
    kernel_pa: usize,
    linked_kernel_pa: usize,
) -> Result<&'static mut [Pte; ENTRIES], BootVmError> {
    let idx = unsafe { KERNEL_L0_COUNT };
    if idx >= MAX_KERNEL_L0 {
        return Err(BootVmError::KernelL0TableExhausted);
    }
    unsafe {
        KERNEL_L0_COUNT += 1;
    }

    let table = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_L0S[idx]) };
    for entry in table.entries.iter_mut() {
        *entry = Pte::invalid();
    }
    let l0_link_pa = unsafe { physical_address_of(core::ptr::addr_of!(KERNEL_L0S[idx]) as usize) };
    let l0_pa = runtime_physical_address(l0_link_pa, kernel_pa, linked_kernel_pa)?;
    l1.entries[l1_index] = Pte::new_table_pa(l0_pa);
    Ok(&mut table.entries)
}

fn runtime_physical_address(
    linked_pa: usize,
    runtime_kernel_pa: usize,
    linked_kernel_pa: usize,
) -> Result<usize, BootVmError> {
    let offset = linked_pa
        .checked_sub(linked_kernel_pa)
        .ok_or(BootVmError::AddressOverflow)?;
    runtime_kernel_pa
        .checked_add(offset)
        .ok_or(BootVmError::AddressOverflow)
}

/// Convert an identity-mapped physical address (or low VA) into its early
/// high-half alias.  The early root maps both addresses to the same leaf.
pub const fn high_alias_of(low_va: usize) -> usize {
    low_va.wrapping_add(HIGH_HALF_OFFSET)
}

/// Convert an early high-half alias back to the identity/physical address.
///
/// This is intentionally only an early-boot helper.  It is not a general
/// virtual-address translation routine; the final address-space backend will
/// own that operation later.
pub const fn low_address_of(high_va: usize) -> usize {
    high_va.wrapping_sub(HIGH_HALF_OFFSET)
}

/// Normalize an early address that may already be a high-half VMA.
pub const fn high_alias_or_self(address: usize) -> usize {
    if address >= HIGH_HALF_OFFSET {
        address
    } else {
        high_alias_of(address)
    }
}

/// Normalize an early address to the identity/physical view.
pub const fn physical_address_of(address: usize) -> usize {
    if address >= HIGH_HALF_OFFSET {
        low_address_of(address)
    } else {
        address
    }
}
