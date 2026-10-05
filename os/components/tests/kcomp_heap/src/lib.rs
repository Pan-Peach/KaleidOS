//! Same artifact and business code in KernelNative and IsolatedNative.
#![no_std]
extern crate alloc;
use alloc::{boxed::Box, vec};
use kcomp_sdk::{Errno, mem};

unsafe extern "C" {
    fn calloc(count: usize, size: usize) -> *mut u8;
    fn realloc(ptr: *mut u8, size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

// Exercise the freestanding C front end through the same deployment adapter.
fn c_allocations() -> i32 {
    // Keep the calls opaque: LLVM's libc allocation folding must not replace
    // the custom freestanding implementation or its overflow checks.
    let calloc = core::hint::black_box(calloc as unsafe extern "C" fn(usize, usize) -> *mut u8);
    let realloc = core::hint::black_box(realloc as unsafe extern "C" fn(*mut u8, usize) -> *mut u8);
    let free = core::hint::black_box(free as unsafe extern "C" fn(*mut u8));
    unsafe {
        let ptr = calloc(17, 3);
        if ptr.is_null() {
            return Errno::ENOMEM.code();
        }
        if (0..51).any(|i| ptr.add(i).read() != 0) {
            free(ptr);
            return Errno::EFAULT.code();
        }
        ptr.write(0xa5);
        let grown = realloc(ptr, 1024);
        if grown.is_null() {
            free(ptr);
            return Errno::EAGAIN.code();
        }
        if grown.read() != 0xa5 {
            free(grown);
            return Errno::EIO.code();
        }
        if !realloc(grown, usize::MAX).is_null() {
            return Errno::EOVERFLOW.code();
        }
        if !calloc(usize::MAX, 2).is_null() {
            free(grown);
            return Errno::ERANGE.code();
        }
        free(grown);
        0
    }
}

#[repr(C)]
struct State {
    // C-layout report prefix for the ArchTest. No Rust layout crosses the ABI.
    buffer: usize,
    len: usize,
    data: alloc::vec::Vec<u8>,
}

kcomp_sdk::kcomp_instance_create!(|args, out_state| {
    let Some(args) = (unsafe { args.as_ref() }) else {
        return Errno::EINVAL.code();
    };
    if out_state.is_null() {
        return Errno::EFAULT.code();
    }
    let seed = if args.config_len == 1 && !args.config.is_null() {
        unsafe { args.config.cast::<u8>().read() }
    } else {
        0x5a
    };
    // Exact backing release, alignment and zeroing, independent of deployment.
    let view = match mem::mem_alloc(1, 8192) {
        Ok(view) => view,
        Err(error) => return error.code(),
    };
    if view.base % 8192 != 0 || view.len < 8192 {
        return Errno::EIO.code();
    }
    let raw = view.base as *mut u8;
    if unsafe { raw.read() } != 0 {
        return Errno::EIO.code();
    }
    unsafe {
        raw.write(0xc3);
    }
    if let Err(error) = mem::mem_release(view) {
        return error.code();
    }
    let c_status = c_allocations();
    if c_status != 0 {
        return c_status;
    }

    let mut data = vec![seed; 8192];
    data.reserve(24576); // Exercise backing growth and realloc.
    if data.iter().any(|byte| *byte != seed) {
        return Errno::EIO.code();
    }
    let state = Box::new(State {
        buffer: data.as_ptr() as usize,
        len: data.len(),
        data,
    });
    unsafe {
        *out_state = Box::into_raw(state).cast();
    }
    0
});

kcomp_sdk::kcomp_instance_destroy!(|state| {
    if !state.is_null() {
        unsafe {
            drop(Box::from_raw(state.cast::<State>()));
        }
    }
    0
});
