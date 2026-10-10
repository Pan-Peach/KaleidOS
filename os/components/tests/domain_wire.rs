//! Raw test-only synchronous execution-domain ABI; no business service or SDK transport.
pub const DOMAIN_CONTRACT: u64 = 0x444F_4D41_494E_5356;
pub const DOMAIN_ABI: u64 = 0x444F_4D47_4154_4553;
pub const DOMAIN_NAME: &[u8] = b"domain.test";
pub const METHOD_CAPACITY: u32 = 0;
pub const METHOD_READ: u32 = 1;
pub const METHOD_WRITE: u32 = 2;
#[repr(C)]
pub struct DomainApi {
    pub capacity_sectors: unsafe extern "C" fn(*mut ()) -> u64,
    pub read: unsafe extern "C" fn(*mut (), u64, *mut u8, usize) -> i32,
    pub write: unsafe extern "C" fn(*mut (), u64, *const u8, usize) -> i32,
}
