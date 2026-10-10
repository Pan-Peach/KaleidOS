//! Private-domain backing, recorded only by exact AS mappings. Heap objects and
//! allocator state remain private to the runtime. This is cooperative S-mode
//! lifetime management, not an adversarial security boundary.
use crate::errno::Errno;
use crate::generated::abi::{KCORE_MEMORY_VIEW_LOCAL_VA, MemoryView};
use crate::memory;
use crate::memory::address_space::{
    self, AddressSpaceHandle, Mapping, MappingPermission, PhysicalRange, VirtualRange,
};

/// Separate from image, stack and ABI windows. Only this range may be released
/// through the component memory ABI; image/stack mappings are Core-owned.
pub const WINDOW: VirtualRange = VirtualRange {
    base: 0x2300_0000,
    size: 0x0c00_0000,
};

pub fn contains(range: VirtualRange) -> bool {
    range.size != 0
        && range.base >= WINDOW.base
        && range
            .base
            .checked_add(range.size)
            .is_some_and(|end| end <= WINDOW.base + WINDOW.size)
}

pub fn acquire(handle: AddressSpaceHandle, size: usize, align: usize) -> Result<MemoryView, Errno> {
    acquire_mode(handle, size, align, false)
}
#[cfg(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu"))]
pub(crate) fn acquire_user(
    handle: AddressSpaceHandle,
    size: usize,
    align: usize,
) -> Result<MemoryView, Errno> {
    acquire_mode(handle, size, align, true)
}
fn acquire_mode(
    handle: AddressSpaceHandle,
    size: usize,
    align: usize,
    user: bool,
) -> Result<MemoryView, Errno> {
    if !address_space::isolation_capable() {
        return Err(Errno::ENOTSUP);
    }
    // Round to an allocator extent large enough for alignment as well as size.
    let lease = memory::alloc_region(size.max(align)).map_err(|_| Errno::ENOMEM)?;
    let physical = PhysicalRange {
        base: lease.base(),
        size: lease.size(),
    };
    let virtual_range = address_space::find_free_range(
        handle,
        WINDOW,
        physical.size,
        align.max(memory::ALLOC_GRANULE),
    )
    .map_err(|_| Errno::ENOMEM)?;
    // SAFETY: fresh allocation, still accessible through its shared identity alias.
    unsafe {
        core::ptr::write_bytes(physical.base as *mut u8, 0, physical.size);
    }
    if address_space::map(
        handle,
        Mapping {
            virtual_range,
            physical_range: physical,
            permission: MappingPermission::READ
                | MappingPermission::WRITE
                | if user {
                    MappingPermission::USER
                } else {
                    MappingPermission::empty()
                },
        },
    )
    .is_err()
    {
        return Err(Errno::ENOMEM);
    }
    // Publish only after the private mapping is ready. On an exclusion failure
    // retain the backing: a partially changed set of roots must never see reuse.
    if memory::kernel_mappings::publish_private_backing(physical).is_err() {
        // No view is published, but the existing AS retains ownership so that
        // explicit component reclaim can find the failed allocation.
        core::mem::forget(lease);
        return Err(Errno::ENOMEM);
    }
    core::mem::forget(lease);
    flush();
    Ok(MemoryView {
        kind: KCORE_MEMORY_VIEW_LOCAL_VA,
        reserved: 0,
        base: virtual_range.base as u64,
        len: virtual_range.size as u64,
    })
}

pub fn release(handle: AddressSpaceHandle, view: MemoryView) -> Result<(), Errno> {
    let range = VirtualRange {
        base: usize::try_from(view.base).map_err(|_| Errno::EINVAL)?,
        size: usize::try_from(view.len).map_err(|_| Errno::EINVAL)?,
    };
    if !contains(range) {
        return Err(Errno::EPERM);
    }
    let mapping = address_space::mapping_exact(handle, &range)
        .map_err(|_| Errno::EPERM)?
        .ok_or(Errno::ENOENT)?;
    // Keep the owning mapping until alias restoration succeeds. Explicit
    // release is a cooperative promise that no task, callback or DMA borrows it.
    memory::kernel_mappings::release_private_backing(mapping.physical_range)
        .map_err(|_| Errno::EIO)?;
    address_space::unmap(handle, &range).map_err(|_| Errno::EIO)?;
    flush();
    memory::free_region_raw(mapping.physical_range.base, mapping.physical_range.size)
        .map_err(|_| Errno::EINVAL)
}

fn flush() {
    #[cfg(all(
        feature = "vm-mmu",
        any(target_arch = "riscv32", target_arch = "riscv64")
    ))]
    // SAFETY: Core is modifying the currently entered instance's mappings.
    unsafe {
        arch::riscv::mmu::flush_tlb();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_window_excludes_core_owned_and_overflowing_ranges() {
        assert!(contains(WINDOW));
        assert!(!contains(VirtualRange {
            base: WINDOW.base,
            size: 0
        }));
        assert!(!contains(
            crate::component::isolated_lifecycle::window_range()
        ));
        assert!(!contains(
            crate::component::isolated_lifecycle::stack_range()
        ));
        assert!(!contains(VirtualRange {
            base: usize::MAX - 4095,
            size: 4096
        }));
    }
}
