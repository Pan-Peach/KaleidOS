//! Flat LE codecs shared by the SDK and VFS service. No layout/padding casts.
pub use crate::generated::vfs_wire::{
    decode_vfs_lookup as lookup, decode_vfs_open_request as open, decode_vfs_path as path,
    encode_vfs_lookup as put_lookup, encode_vfs_open_request as put_open,
    encode_vfs_path as put_path,
};
pub fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
pub fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
pub fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
pub fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
