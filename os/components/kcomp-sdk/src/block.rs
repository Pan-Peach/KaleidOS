//! BlockDevice Contract identity and generated IPC business interface.
pub use crate::generated::block::KCOMP_BLOCK_DEVICE_NAME as BLOCK_DEVICE_NAME;
pub use crate::generated::block_wire::Provider as BlockDeviceProvider;
use crate::{abi::InterfaceKind, endpoint::Contract};
pub struct BlockDevice;
impl Contract for BlockDevice {
    const ID: u64 = crate::generated::block::KCOMP_BLOCK_DEVICE_CONTRACT;
    const ABI: u64 = crate::generated::block::KCOMP_BLOCK_DEVICE_ABI;
    const KIND: InterfaceKind = InterfaceKind::Device;
}
pub mod client;
pub mod server;
