//! Test-only Native contract, compiled privately into provider and consumer.
//! Mailbox is a single-consumer Runtime protocol, not a Core object or ABI.
pub const CONFIG_ABI: u64 = 0x4348_4543_4B43_4647;
pub const CONTRACT: u64 = 0x4348_4543_4B53_554D;
pub const ABI: u64 = 0xF17C_392A_6518_D0B4;
pub const NAME: &[u8] = b"checksum";
pub const PASSIVE: u32 = 0;
pub const ACTIVE: u32 = 1;
pub const HYBRID: u32 = 2;
pub const GATE_ONLY: u32 = 3;
pub const LIFECYCLE_PROBE: u32 = 4;
pub const ECHO: u32 = 1;
pub const ECHO_MAX: usize = 512;

/// phase: 0 empty, 1 producer reserved, 2 request, 3 reply.
/// All shared words are accessed through AtomicU32::from_ptr, on both sides.
#[repr(C)]
pub struct Mailbox {
    pub phase: u32,
    pub value: u32,
    pub result: u32,
    pub direct_calls: u32,
    pub worker_calls: u32,
    pub stop: u32,
}

#[repr(C)]
pub struct Api {
    pub checksum: extern "C" fn(*mut (), u32) -> u32,
    pub mailbox: extern "C" fn(*mut ()) -> *mut Mailbox,
    pub worker: extern "C" fn(*mut ()) -> u32,
    pub echo: unsafe extern "C" fn(*mut (), *const u8, *mut u8, usize) -> i32,
}
