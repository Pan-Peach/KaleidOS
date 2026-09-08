//! RISC-V early boot page-table plan.
//!
//! This is the small, statically backed mapping used before Core's physical
//! allocator exists.  It deliberately keeps both the identity window and the
//! high-half alias alive.  A later final-kernel mapping will replace this root
//! after memory discovery and allocation are available.

use super::sv39::{PAGE_SIZE, PageTable, Pte, PteFlags, vpn};

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
const KERNEL_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::X)
    .union(PteFlags::A)
    .union(PteFlags::D);

/// Early root storage. It lives in the image's BSS and is never used as the
/// future per-domain AddressSpace root.
static mut BOOT_ROOT: PageTable = PageTable::empty();
static mut KERNEL_L1: PageTable = PageTable::empty();
static mut KERNEL_L0: PageTable = PageTable::empty();
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
    RootIndexConflict,
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

    // The first version uses one level-0 table for the fixed kernel VMA.
    // Keep the image within one 2 MiB level-1 slot until a multi-table
    // mapping is needed.
    if image_size > MEGAPAGE_SIZE {
        return Err(BootVmError::KernelMappingTooLarge);
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
        install_kernel_alias(root, kernel_pa, linked_kernel_pa, image_size)?;

        let linked_root_pa = physical_address_of(root as *const PageTable as usize);
        BOOT_ROOT_PA = runtime_physical_address(linked_root_pa, kernel_pa, linked_kernel_pa)?;
    }

    Ok(())
}

unsafe fn install_kernel_alias(
    root: &mut PageTable,
    kernel_pa: usize,
    linked_kernel_pa: usize,
    image_size: usize,
) -> Result<(), BootVmError> {
    let last_kernel_va = KERNEL_VMA
        .checked_add(image_size - 1)
        .ok_or(BootVmError::AddressOverflow)?;
    if vpn(KERNEL_VMA, 1) != vpn(last_kernel_va, 1) {
        return Err(BootVmError::KernelMappingTooLarge);
    }

    let l1_link_pa = physical_address_of(core::ptr::addr_of!(KERNEL_L1) as usize);
    let l0_link_pa = physical_address_of(core::ptr::addr_of!(KERNEL_L0) as usize);
    let l1_pa = runtime_physical_address(l1_link_pa, kernel_pa, linked_kernel_pa)?;
    let l0_pa = runtime_physical_address(l0_link_pa, kernel_pa, linked_kernel_pa)?;
    let l1 = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_L1) };
    let l0 = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL_L0) };

    // Preserve the original 1 GiB high alias as 2 MiB leaves, then replace
    // the one slot occupied by KERNEL_VMA with a 4 KiB table for the image.
    let linked_window = linked_kernel_pa & !(GIGAPAGE_SIZE - 1);
    for (index, entry) in l1.entries.iter_mut().enumerate() {
        let pa = linked_window
            .checked_add(index * MEGAPAGE_SIZE)
            .ok_or(BootVmError::AddressOverflow)?;
        *entry = Pte::new_leaf_pa(pa, IDENTITY_FLAGS);
    }
    for entry in l0.entries.iter_mut() {
        *entry = Pte::invalid();
    }

    root.entries[vpn(KERNEL_VMA, 2)] = Pte::new_table_pa(l1_pa);
    l1.entries[vpn(KERNEL_VMA, 1)] = Pte::new_table_pa(l0_pa);

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
        l0.entries[vpn(va, 0)] = Pte::new_leaf_pa(pa, KERNEL_FLAGS);
    }

    Ok(())
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
