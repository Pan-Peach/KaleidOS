//! Suspend a cooperative private S-mode invocation before entering scheduling
//! Core APIs. Both the callback record and stack belong to Core; no lock or
//! private-stack reference survives the root transition.
#[cfg(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
mod implementation {
    use super::super::containment::cross_as;
    use arch::{
        CpuArch, CpuImpl,
        riscv::trampoline::{self, Context},
    };

    pub fn active() -> bool {
        cross_as::active_cross_as().is_some()
    }

    pub fn on_core<R, F: FnOnce() -> R + 'static>(f: F) -> Result<R, crate::errno::Errno> {
        struct Call<F, R> {
            function: Option<F>,
            result: Option<R>,
        }
        extern "C" fn dispatch<F: FnOnce() -> R, R>(
            pointer: usize,
            _: usize,
            _: usize,
            _: usize,
        ) -> usize {
            let call = unsafe { &mut *(pointer as *mut Call<F, R>) };
            call.result = Some(call.function.take().unwrap()());
            0
        }
        let cross = cross_as::active_cross_as().ok_or(crate::errno::Errno::EPERM)?;
        let flags = CpuImpl::disable_irq();
        let result = (|| {
            let stack =
                crate::memory::alloc_region(32 * 1024).map_err(|_| crate::errno::Errno::ENOMEM)?;
            let bytes = core::mem::size_of::<Call<F, R>>();
            if bytes > 1024 || core::mem::align_of::<Call<F, R>>() > 16 {
                return Err(crate::errno::Errno::EINVAL);
            }
            let task = crate::resource::RequestContext::ambient()
                .and_then(|c| c.task)
                .filter(|id| crate::sched::current_task() == Some(*id));
            // A failed suspended call never unwinds. Keep its Core stack in the
            // existing TaskRecord so confirmed teardown can reclaim it later.
            let (base, size) = (stack.base(), stack.size());
            let mut transient = Some(stack);
            if let Some(id) = task {
                let mut table = crate::task::get_task_table().lock();
                let record = table.get_mut(id).unwrap();
                assert!(record.api_stack.is_none());
                record.api_stack = transient.take();
            }
            let call = base as *mut Call<F, R>;
            let context = (base + 1024) as *mut Context;
            unsafe {
                call.write(Call {
                    function: Some(f),
                    result: None,
                });
                context.write((&*(*cross).context.cast::<Context>()).return_call(
                    dispatch::<F, R> as *const () as usize,
                    base + size,
                    call as usize,
                ));
            }
            let previous = cross_as::swap_cross_as(core::ptr::null_mut());
            trampoline::enter(unsafe { &mut *context });
            cross_as::restore_cross_as(previous);
            let call = unsafe { call.read() };
            if let Some(id) = task {
                drop(
                    crate::task::get_task_table()
                        .lock()
                        .get_mut(id)
                        .unwrap()
                        .api_stack
                        .take(),
                );
            }
            drop(transient);
            call.result.ok_or(crate::errno::Errno::EIO)
        })();
        CpuImpl::restore_irq(flags);
        result
    }
}
#[cfg(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
pub(crate) use implementation::*;
#[cfg(not(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
)))]
pub(crate) fn active() -> bool {
    false
}
#[cfg(not(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
)))]
pub(crate) fn on_core<R>(_: impl FnOnce() -> R + 'static) -> Result<R, crate::errno::Errno> {
    Err(crate::errno::Errno::ENOTSUP)
}
