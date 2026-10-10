//! CPU-only private-domain teardown. No new ownership ledger: image lease,
//! exact AS mappings, backend table frames and existing TaskRecords own truth.
use super::{ComponentId, registry};
use crate::{errno::Errno, task};

/// Revoke immediately; saved execution can be canceled, Running execution must
/// acknowledge on its CPU. U returns on a bounded timer slice. S-mode remains
/// cooperative: an unyielding/IRQ-masked Task returns EBUSY and retains backing.
pub fn force_stop(id: ComponentId) -> Result<(), Errno> {
    if registry::get_registry().lock().get(id).is_none() {
        return Err(Errno::ENOENT);
    }
    if registry::get_registry()
        .lock()
        .get(id)
        .is_some_and(|r| r.state != super::ComponentState::Stopped)
    {
        super::fail_component(id, super::load::ComponentLoadError::CallerNotReady);
    }
    let registry = registry::get_registry().lock();
    let native_raw_exports = registry.get(id).is_some_and(|r| {
        r.execution_domain == super::endpoint::ExecutionDomain::KernelNative
            && super::endpoint::get_endpoints()
                .lock()
                .has_direct_exports(id)
    });
    let mut table = task::get_task_table().lock();
    table.stop_saved(id);
    let running = registry.get(id).is_some_and(|r| r.inflight != 0)
        || table.iter().any(|(_, r)| {
            r.owner() == id && (r.state() != task::TaskState::Exited || !r.execution_retired)
        });
    drop(table);
    drop(registry);
    if running {
        return Err(Errno::EBUSY);
    }
    // Copied native tables can still execute on another owner's Task. Logical
    // invalidation cannot revoke those pointers or certify execution drain.
    if native_raw_exports {
        return Err(Errno::ENOTSUP);
    }
    Ok(())
}

pub fn reclaim(id: ComponentId) -> Result<(), Errno> {
    #[cfg(all(
        feature = "vm-mmu",
        feature = "supervisor",
        any(target_arch = "riscv32", target_arch = "riscv64")
    ))]
    {
        implementation::reclaim(id)
    }
    #[cfg(not(all(
        feature = "vm-mmu",
        feature = "supervisor",
        any(target_arch = "riscv32", target_arch = "riscv64")
    )))]
    {
        let _ = id;
        Err(Errno::ENOTSUP)
    }
}
#[cfg(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
mod implementation {
    use super::*;
    use crate::component::{ComponentState, endpoint::ExecutionDomain};
    use crate::{
        irq::IrqSaveGuard,
        memory::{
            self,
            address_space::{self as spaces, PhysicalRange},
        },
    };
    pub(super) fn reclaim(id: ComponentId) -> Result<(), Errno> {
        let _irq = IrqSaveGuard::new();
        let mut registry = registry::get_registry().lock();
        let record = registry.get(id).ok_or(Errno::ENOENT)?;
        if record.reclaimed {
            return Ok(());
        }
        if record.execution_domain == ExecutionDomain::KernelNative {
            return Err(Errno::ENOTSUP);
        }
        if !matches!(
            record.state,
            ComponentState::Stopped | ComponentState::Failed
        ) || record.inflight != 0
        {
            return Err(Errno::EBUSY);
        }
        let space = record.address_space;
        let image = record
            .loaded
            .memory
            .as_ref()
            .ok_or(Errno::ENOTSUP)?
            .region();
        let mut table = task::get_task_table().lock();
        if table.iter().any(|(_, r)| {
            r.owner() == id && (r.state() != task::TaskState::Exited || !r.execution_retired)
        }) {
            return Err(Errno::EBUSY);
        }
        // Alias restoration edits every private root. The first implementation
        // requires a global private-domain safe point rather than pretending a
        // local sfence invalidates another CPU. Entry/return always fully flush.
        if registry.iter().any(|r| {
            r.id != id
                && r.execution_domain != ExecutionDomain::KernelNative
                && (matches!(r.state, ComponentState::Starting | ComponentState::Stopping)
                    || r.inflight != 0)
        }) || table.iter().any(|(_, r)| {
            matches!(r.state(), task::TaskState::Running(_))
                && registry
                    .get(r.owner())
                    .is_some_and(|owner| owner.execution_domain != ExecutionDomain::KernelNative)
        }) {
            return Err(Errno::EBUSY);
        }
        let mappings = match space {
            Some(space) => spaces::private_mappings(space).map_err(|_| Errno::EIO)?,
            None => alloc::vec::Vec::new(), // declaration succeeded; root creation failed
        };
        let mut extents = alloc::vec::Vec::new();
        extents
            .try_reserve(mappings.len() + 1)
            .map_err(|_| Errno::ENOMEM)?;
        extents.push(image);
        for mapping in mappings {
            let range = mapping.physical_range;
            if range.base >= image.base
                && range
                    .base
                    .checked_add(range.size)
                    .is_some_and(|end| end <= image.base + image.size)
            {
                continue;
            }
            if range.size == 0
                || !range.size.is_power_of_two()
                || !range.base.is_multiple_of(memory::ALLOC_GRANULE)
            {
                return Err(Errno::ENOTSUP);
            }
            if extents.iter().any(|other: &PhysicalRange| {
                range.base < other.base + other.size && other.base < range.base + range.size
            }) {
                return Err(Errno::ENOTSUP);
            }
            extents.push(range);
        }
        let ids: alloc::vec::Vec<_> = table
            .iter()
            .filter(|(_, r)| r.owner() == id)
            .map(|(id, _)| *id)
            .collect();
        // Retire before restoration so this root is not populated again. Any
        // restoration failure retains every allocation and permits retry.
        if let Some(space) = space {
            spaces::retire(space).map_err(|_| Errno::EIO)?;
        }
        for extent in &extents {
            memory::kernel_mappings::release_private_backing(*extent).map_err(|_| Errno::EIO)?;
        }
        if let Some(space) = space {
            unsafe {
                spaces::reclaim(space).map_err(|_| Errno::EIO)?;
            }
        }
        for task in ids {
            table.remove(task).map_err(Errno::from)?;
        }
        for extent in extents {
            memory::free_region_raw(extent.base, extent.size).expect("owned private backing");
        }
        registry.finish_reclaim(id);
        Ok(())
    }
}
