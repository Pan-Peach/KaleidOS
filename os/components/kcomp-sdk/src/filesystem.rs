//! Filesystem Contract identity and generated IPC business interface.
pub use crate::generated::filesystem::{
    KCOMP_FILESYSTEM_NAME as FILESYSTEM_NAME, KCOMP_FILESYSTEM_OPEN_READ as FILESYSTEM_OPEN_READ,
};
pub use crate::generated::filesystem_wire::Provider as FileSystemProvider;
use crate::{abi::InterfaceKind, endpoint::Contract};
pub struct FileSystem;
impl Contract for FileSystem {
    const ID: u64 = crate::generated::filesystem::KCOMP_FILESYSTEM_CONTRACT;
    const ABI: u64 = crate::generated::filesystem::KCOMP_FILESYSTEM_ABI;
    const KIND: InterfaceKind = InterfaceKind::Service;
}
pub mod client;
