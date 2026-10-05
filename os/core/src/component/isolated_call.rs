//! Isolated outbound Gate: copy flat buffers in the caller's AS, dispatch on a
//! Core-owned stack in the suspended Core root, then restore the caller and copy
//! output back. No caller-private pointer reaches a provider. Buffers are borrowed
//! for this synchronous call only; payload bytes are never interpreted by Core.

use super::{ComponentId, call::CallError, endpoint::EndpointId};
use crate::{generated::abi::KcompCallFrame, task::TaskId};

#[cfg(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
mod imp {
    use super::super::{call, containment::cross_as, registry};
    use super::*;
    use crate::memory::{
        self,
        address_space::{self, AddressSpaceHandle, MappingPermission, VirtualRange},
    };
    use alloc::vec::Vec;
    use arch::{
        CpuArch, CpuImpl,
        riscv::{
            mmu,
            trampoline::{self, Context, Outcome},
        },
    };

    struct Request {
        caller: ComponentId,
        task: Option<TaskId>,
        endpoint: EndpointId,
        method: u32,
        args: Vec<u8>,
        input: Vec<u8>,
        output: Vec<u8>,
        status: i32,
        result: Result<(), CallError>,
    }

    // Records live below the callback stack, and remain mapped in both roots.
    const RECORD_BYTES: usize = 1024;
    const _: () =
        assert!(core::mem::size_of::<Request>() + core::mem::size_of::<Context>() < RECORD_BYTES);

    fn validate(
        handle: AddressSpaceHandle,
        ptr: usize,
        len: usize,
        permission: MappingPermission,
    ) -> Result<(), CallError> {
        if len > isize::MAX as usize
            || !address_space::range_has_permission(
                handle,
                VirtualRange {
                    base: ptr,
                    size: len,
                },
                permission,
            )
        {
            return Err(CallError::InvalidFrame);
        }
        Ok(())
    }

    fn copy_buffer(ptr: *const u8, len: usize) -> Result<Vec<u8>, CallError> {
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(len)
            .map_err(|_| CallError::NoServiceStack)?;
        if len != 0 {
            // SAFETY: caller-AS access was validated and it remains active;
            // synchronous, IRQ-disabled execution prevents mapping changes.
            buffer.extend_from_slice(unsafe { core::slice::from_raw_parts(ptr, len) });
        }
        Ok(buffer)
    }

    extern "C" fn dispatch(request: usize, _: usize, _: usize, _: usize) -> usize {
        // SAFETY: Core owns this record below the callback's shared stack.
        let request = unsafe { &mut *(request as *mut Request) };
        let frame = KcompCallFrame {
            args: request.args.as_ptr(),
            args_len: request.args.len(),
            input: request.input.as_ptr(),
            input_len: request.input.len(),
            output: request.output.as_mut_ptr(),
            output_len: request.output.len(),
        };
        request.result = call::dispatch(
            Some(request.caller),
            request.task,
            request.endpoint,
            request.method,
            &frame,
            &mut request.status,
        );
        0
    }

    fn call_inner(
        caller: ComponentId,
        task: Option<TaskId>,
        endpoint: EndpointId,
        method: u32,
        frame: &KcompCallFrame,
        out_status: *mut i32,
    ) -> Result<(), CallError> {
        let handle = registry::get_registry()
            .lock()
            .get(caller)
            .and_then(|record| record.address_space)
            .ok_or(CallError::UnsupportedCallerDomain)?;
        let cross = cross_as::active_cross_as().ok_or(CallError::UnsupportedCallerDomain)?;
        // SAFETY: the CPU-local record belongs to a suspended, live invocation.
        let cross = unsafe { &*cross };
        if cross.space != Some(handle) || cross.expected_satp != mmu::current_satp() {
            return Err(CallError::UnsupportedCallerDomain);
        }
        validate(
            handle,
            frame.args as usize,
            frame.args_len,
            MappingPermission::READ,
        )?;
        validate(
            handle,
            frame.input as usize,
            frame.input_len,
            MappingPermission::READ,
        )?;
        validate(
            handle,
            frame.output as usize,
            frame.output_len,
            MappingPermission::READ | MappingPermission::WRITE,
        )?;
        validate(
            handle,
            out_status as usize,
            core::mem::size_of::<i32>(),
            MappingPermission::WRITE,
        )?;
        let request = Request {
            caller,
            task,
            endpoint,
            method,
            args: copy_buffer(frame.args, frame.args_len)?,
            input: copy_buffer(frame.input, frame.input_len)?,
            output: copy_buffer(frame.output, frame.output_len)?,
            status: 0,
            result: Err(CallError::UnsupportedCallerDomain),
        };
        let stack = memory::alloc_region(32 * 1024).map_err(|_| CallError::NoServiceStack)?;
        let request_ptr = stack.base() as *mut Request;
        let context_ptr =
            (stack.base() + RECORD_BYTES - core::mem::size_of::<Context>()) as *mut Context;
        // Every production Isolated entry is made from Core; its saved caller
        // root is authoritative. Nested outbound calls also enter via this bridge.
        // SAFETY: cross points to the original Core trampoline's live record.
        let context = unsafe { &*cross.context.cast::<Context>() }.return_call(
            dispatch as *const () as usize,
            stack.base() + stack.size(),
            request_ptr as usize,
        );
        // SAFETY: disjoint aligned records below the stack, shared in both roots.
        unsafe {
            request_ptr.write(request);
            context_ptr.write(context);
        }
        // Suspend the caller's cross-AS escape target while running Core/native
        // code. A native provider panic must escape its own service guard, never
        // abandon the suspended Isolated caller. Isolated providers install theirs.
        let previous = cross_as::swap_cross_as(core::ptr::null_mut());
        let outcome = trampoline::enter(unsafe { &mut *context_ptr });
        cross_as::restore_cross_as(previous);
        // SAFETY: bridge returned into the original root; move ownership back so
        // Vecs are dropped normally even when dispatch fails.
        let request = unsafe { request_ptr.read() };
        debug_assert_eq!(outcome, Outcome::Returned(0));
        request.result?;
        if frame.output_len != 0 {
            // SAFETY: caller mapping restored; no provider retained a borrow.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    request.output.as_ptr(),
                    frame.output,
                    frame.output_len,
                );
            }
        }
        // SAFETY: writable range validated before dispatch, caller AS restored.
        unsafe {
            core::ptr::write_unaligned(out_status, request.status);
        }
        Ok(())
    }

    pub(crate) fn call(
        caller: ComponentId,
        task: Option<TaskId>,
        endpoint: EndpointId,
        method: u32,
        frame: &KcompCallFrame,
        out_status: *mut i32,
    ) -> Result<(), CallError> {
        let flags = CpuImpl::disable_irq();
        let result = call_inner(caller, task, endpoint, method, frame, out_status);
        CpuImpl::restore_irq(flags);
        result
    }
}

#[cfg(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
pub(super) use imp::call;

#[cfg(not(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
)))]
pub(super) fn call(
    _: ComponentId,
    _: Option<TaskId>,
    _: EndpointId,
    _: u32,
    _: &KcompCallFrame,
    _: *mut i32,
) -> Result<(), CallError> {
    Err(CallError::UnsupportedCallerDomain)
}
