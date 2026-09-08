//! RISC-V early boot page-table plan.
//!
//! This is the small, statically backed mapping used before Core's physical
//! allocator exists.  It deliberately keeps both the identity window and the
//! high-half alias alive.  A later final-kernel mapping will replace this root
//! after memory discovery and allocation are available.

use super::sv39::{PageTable, Pte, PteFlags};

pub const HIGH_HALF_OFFSET: usize = 0xffff_ffc0_0000_0000;
pub const GIGAPAGE_SIZE: usize = 1 << 30;

const IDENTITY_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::X)
    .union(PteFlags::A)
    .union(PteFlags::D);
const MMIO_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::A)
    .union(PteFlags::D);

/// Early root storage. It lives in the image's BSS and is never used as the
/// future per-domain AddressSpace root.
static mut BOOT_ROOT: PageTable = PageTable::empty();

pub fn root() -> &'static mut PageTable {
    // SAFETY: boot is single-hart at this point and the root is exclusively
    // owned by the bootstrap path until the final root replaces it.
    unsafe { &mut *core::ptr::addr_of_mut!(BOOT_ROOT) }
}

pub fn root_pa() -> usize {
    root() as *const PageTable as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootVmError {
    EmptyRam,
    AddressOverflow,
    KernelOutsideRam,
    RootIndexConflict,
}

/// Install the temporary Linux-shaped boot mapping.
///
/// - root[0]      : low MMIO window
/// - root[ram_root_index] : identity-mapped RAM windows
/// - root[high_root_index] : high-half aliases of those windows
///
/// The kernel still executes through the identity VA after this call. The
/// alias is only consumed by the optional transition probe for now.
///
/// `ram_base` and `ram_size` come from bootstrap discovery (currently FDT).
/// The mapping is deliberately coarse: one level-2 leaf per 1 GiB window.
/// The final address space will replace this root with 4 KiB-granular tables.
pub unsafe fn init(
    kernel_pa: usize,
    ram_base: usize,
    ram_size: usize,
) -> Result<(), BootVmError> {
    if ram_size == 0 {
        return Err(BootVmError::EmptyRam);
    }

    let ram_end = ram_base
        .checked_add(ram_size)
        .ok_or(BootVmError::AddressOverflow)?;
    if kernel_pa < ram_base || kernel_pa >= ram_end {
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

    Ok(())
}

pub const fn high_alias_of(low_va: usize) -> usize {
    low_va.wrapping_add(HIGH_HALF_OFFSET)
}
