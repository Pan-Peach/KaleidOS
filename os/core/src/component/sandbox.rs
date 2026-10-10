//! RV64 U-mode component deployment adapter. Business C ABI and IPC envelopes
//! are unchanged: imports resolve to private executable ecall thunks. Actual
//! identity comes from the suspended Core boundary, never syscall arguments.
#[cfg(all(
    feature = "supervisor",
    feature = "vm-mmu",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
use crate::memory::address_space::AddressSpaceHandle;
#[cfg(all(
    feature = "supervisor",
    feature = "vm-mmu",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
use crate::memory::address_space::VirtualRange;
pub(crate) const STUB_BASE: usize = 0x2210_0000;
pub(crate) const IMPORTS: &[&[u8]] = &[
    b"kcore_log_line",
    b"kcore_console_write_byte",
    b"kcore_now",
    b"kcore_timebase_hz",
    b"kcore_component_current",
    b"kcore_cpu_current",
    b"kcore_memory_acquire",
    b"kcore_memory_release",
    b"kcore_task_create",
    b"kcore_task_start",
    b"kcore_task_start_on",
    b"kcore_task_yield",
    b"kcore_task_exit",
    b"kcore_task_stop_requested",
    b"kcore_panic_escape",
    b"kcore_endpoint_publish",
    b"kcore_endpoint_lookup",
    b"kcore_endpoint_validate",
    b"kcore_ipc_listen",
    b"kcore_ipc_grant",
    b"kcore_ipc_submit",
    b"kcore_ipc_receive",
    b"kcore_ipc_reply",
    b"kcore_ipc_collect",
    b"kcore_ipc_wait",
    b"kcore_ipc_cancel",
    b"kcore_ipc_close",
];
pub(crate) fn resolve_import(name: &[u8]) -> Option<usize> {
    IMPORTS
        .iter()
        .position(|n| *n == name)
        .map(|index| STUB_BASE + (index + 1) * 12)
}

#[cfg(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu"))]
mod implementation {
    use super::*;
    use crate::memory::address_space::{self as spaces, Mapping, MappingPermission as P};
    use crate::{
        component::{isolated, registry},
        errno::Errno,
        irq::IrqSaveGuard,
        memory, sched,
    };
    use arch::{
        Timer, TimerImpl,
        riscv::user::{UserContext, UserFrame},
    };
    use core::sync::atomic::{AtomicUsize, Ordering};
    static ACTIVE: [AtomicUsize; crate::machine::MAX_CPUS] =
        [const { AtomicUsize::new(0) }; crate::machine::MAX_CPUS];

    pub(crate) fn map_stubs(space: AddressSpaceHandle) -> Result<(), Errno> {
        let lease = memory::alloc_region(4096).map_err(|_| Errno::ENOMEM)?;
        unsafe {
            core::ptr::write_bytes(lease.base() as *mut u8, 0, lease.size());
        }
        for index in 0..=IMPORTS.len() {
            // t0 is caller-saved. Preserve all eight argument registers, including
            // receive's seventh C argument; no separate message descriptor.
            let code = [((index as u32) << 20) | 0x293, 0x0000_0073, 0x0000_8067];
            unsafe {
                core::ptr::copy_nonoverlapping(
                    code.as_ptr().cast::<u8>(),
                    (lease.base() + index * 12) as *mut u8,
                    12,
                );
            }
        }
        let mapping = Mapping {
            virtual_range: VirtualRange {
                base: STUB_BASE,
                size: 4096,
            },
            physical_range: lease.region(),
            permission: P::READ | P::EXECUTE | P::USER,
        };
        spaces::map(space, mapping).map_err(|_| Errno::ENOMEM)?;
        if memory::kernel_mappings::publish_private_backing(lease.region()).is_err() {
            // Preserve the exact extent in the existing AS on failed publish.
            core::mem::forget(lease);
            return Err(Errno::ENOMEM);
        }
        core::mem::forget(lease);
        Ok(())
    }

    pub(crate) fn invoke(
        space: AddressSpaceHandle,
        pc: usize,
        stack: VirtualRange,
        args: [usize; 4],
    ) -> isolated::Outcome {
        if !spaces::private_range_has_permission(
            space,
            VirtualRange { base: pc, size: 2 },
            P::READ | P::EXECUTE | P::USER,
        ) || !spaces::private_range_has_permission(space, stack, P::READ | P::WRITE | P::USER)
        {
            return isolated::Outcome::Faulted;
        }
        let Ok(activation) = spaces::prepare_activation(space) else {
            return isolated::Outcome::Faulted;
        };
        let mut frame = UserFrame::default();
        frame.pc = pc;
        frame.x[1] = STUB_BASE; // returning from create/destroy/Task is an ecall.
        frame.x[2] = stack.base + stack.size;
        frame.x[10..14].copy_from_slice(&args);
        let owner = crate::resource::RequestContext::ambient()
            .unwrap()
            .component;
        let task = sched::current_task();
        let task_boundary = crate::resource::RequestContext::ambient().is_some_and(|c| {
            c.task == task && !crate::component::containment::task_switch_forbidden()
        });
        let limit = TimerImpl::now().saturating_add(
            crate::machine::committed()
                .and_then(|m| m.timebase_frequency)
                .map_or(0, |hz| hz.get()),
        );
        isolated::install();
        loop {
            // Stopping destroy is permitted; a Failed component never resumes U.
            if registry::get_registry().lock().is_failed(owner) {
                return isolated::Outcome::Faulted;
            }
            let (cause, address) = {
                let _irq = IrqSaveGuard::new();
                let deadline = TimerImpl::now().saturating_add(
                    (crate::machine::committed()
                        .and_then(|m| m.timebase_frequency)
                        .map_or(0, |hz| hz.get())
                        / 100)
                        .max(1),
                );
                let Ok(_deadline) = crate::timer::execution_deadline(deadline) else {
                    return isolated::Outcome::Faulted;
                };
                let mut context = UserContext::new(
                    activation.token().satp(),
                    &mut frame,
                    task.map_or(u32::MAX, |id| id.raw()),
                );
                let slot = &ACTIVE[crate::smp::current_cpu().raw()];
                assert_eq!(
                    slot.swap(&mut context as *mut _ as usize, Ordering::AcqRel),
                    0
                );
                unsafe {
                    arch::riscv::user::run(&mut context);
                }
                slot.store(0, Ordering::Release);
                (context.cause, context.address)
            };
            if cause >> 63 != 0 {
                if task_boundary {
                    let _ = sched::yield_current();
                } else if TimerImpl::now() >= limit {
                    return isolated::Outcome::Faulted;
                }
                continue;
            }
            if cause != 8 {
                crate::log!(
                    "component",
                    "U fault owner={} cause={} pc={:#x} address={:#x}",
                    owner.raw(),
                    cause,
                    frame.pc,
                    address
                );
                return isolated::Outcome::Faulted;
            }
            frame.pc += 4;
            let number = frame.x[5];
            if number == 0 {
                return isolated::Outcome::Returned(frame.x[10]);
            }
            let Some(name) = IMPORTS.get(number - 1) else {
                frame.x[10] = Errno::ENOSYS.code() as usize;
                continue;
            };
            if *name == b"kcore_panic_escape" {
                return isolated::Outcome::Faulted;
            }
            if *name == b"kcore_task_exit" {
                if task_boundary {
                    return isolated::Outcome::Returned(0);
                }
                frame.x[10] = Errno::EINVAL.code() as usize;
                continue;
            }
            let mut args = [0; 8];
            args.copy_from_slice(&frame.x[10..18]);
            frame.x[10] = crate::component::export::sandbox_dispatch(name, args);
        }
    }

    pub(crate) unsafe fn on_trap(
        frame: *mut arch::riscv::trap::TrapFrame,
        cause: usize,
        address: usize,
    ) -> bool {
        let trap = unsafe { &*frame };
        if trap.status & 0x100 != 0 {
            return false;
        }
        let pointer = ACTIVE[crate::smp::current_cpu().raw()].load(Ordering::Acquire);
        if pointer == 0 {
            return false;
        }
        let context = unsafe { &*(pointer as *const UserContext) };
        if sched::current_task().map_or(u32::MAX, |id| id.raw()) != context.task
            || arch::riscv::mmu::current_satp() != context.satp
        {
            return false;
        }
        unsafe { arch::riscv::user::stop(pointer as *mut UserContext, trap, cause, address) }
    }
}
#[cfg(all(target_arch = "riscv64", feature = "supervisor", feature = "vm-mmu"))]
pub(crate) use implementation::*;
#[cfg(all(target_arch = "riscv32", feature = "supervisor", feature = "vm-mmu"))]
pub(crate) fn map_stubs(_: AddressSpaceHandle) -> Result<(), crate::errno::Errno> {
    Err(crate::errno::Errno::ENOTSUP)
}
#[cfg(all(target_arch = "riscv32", feature = "supervisor", feature = "vm-mmu"))]
pub(crate) fn invoke(
    _: AddressSpaceHandle,
    _: usize,
    _: VirtualRange,
    _: [usize; 4],
) -> super::isolated::Outcome {
    super::isolated::Outcome::Faulted
}
