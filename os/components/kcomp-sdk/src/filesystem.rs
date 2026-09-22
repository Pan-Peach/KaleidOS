//! `filesystem` service contract shared by VFS and filesystem providers.
//!
//! The first contract is deliberately small and read-only.  A provider keeps
//! its implementation objects private; callers receive only an opaque u64
//! file handle.

use crate::binding::{InterfaceAbi, InterfaceKind, Service};

/// Stable endpoint name for the singleton filesystem service.
pub const FILESYSTEM_NAME: &[u8] = b"filesystem";

/// Exact ABI fingerprint for `FileSystemApi`.
pub const FILESYSTEM_ABI: InterfaceAbi = InterfaceAbi::from_raw(0x4649_4C45_5359_5354);

/// Read-only open flag.  This is a KaleidOS ABI value, not a FatFs `FA_*` value.
pub const FILESYSTEM_OPEN_READ: u32 = 0x0000_0001;

/// `filesystem` provider/consumer function table.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileSystemApi {
    pub mount: unsafe extern "C" fn(ctx: *mut ()) -> i32,
    pub unmount: unsafe extern "C" fn(ctx: *mut ()) -> i32,
    pub open: unsafe extern "C" fn(
        ctx: *mut (),
        path: *const u8,
        flags: u32,
        out_handle: *mut u64,
    ) -> i32,
    pub close: unsafe extern "C" fn(ctx: *mut (), handle: u64) -> i32,
    pub read: unsafe extern "C" fn(
        ctx: *mut (),
        handle: u64,
        buf: *mut u8,
        len: usize,
        out_read: *mut usize,
    ) -> i32,
}

const _: () = {
    assert!(core::mem::size_of::<FileSystemApi>() == 5 * core::mem::size_of::<usize>());
    assert!(core::mem::align_of::<FileSystemApi>() == core::mem::align_of::<usize>());
};

/// The generic filesystem contract.  Concrete providers include FatFs, Ext4,
/// and Tmpfs; the VFS binds this contract without knowing the format.
pub struct FileSystem;

impl Service for FileSystem {
    const NAME: &'static [u8] = FILESYSTEM_NAME;
    const KIND: InterfaceKind = InterfaceKind::Service;
    const ABI: InterfaceAbi = FILESYSTEM_ABI;
    type Api = FileSystemApi;
}
