//! Flat LE codecs shared by the SDK and VFS service. No layout/padding casts.
use super::*;
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
pub fn path(bytes: &[u8]) -> VfsPath {
    VfsPath {
        mount: u64_at(bytes, 0),
        entry: u64_at(bytes, 8),
        fs: u64_at(bytes, 16),
        node: u64_at(bytes, 24),
    }
}
pub fn put_path(bytes: &mut [u8], value: &VfsPath) {
    for (index, word) in [value.mount, value.entry, value.fs, value.node]
        .into_iter()
        .enumerate()
    {
        put64(bytes, index * 8, word);
    }
}
pub fn lookup(bytes: &[u8]) -> VfsLookup {
    VfsLookup {
        start: path(bytes),
        root: path(&bytes[32..]),
        flags: u32_at(bytes, 64),
        max_symlinks: u32_at(bytes, 68),
        encoding: u32_at(bytes, 72),
        reserved: u32_at(bytes, 76),
    }
}
pub fn put_lookup(bytes: &mut [u8], value: &VfsLookup) {
    put_path(bytes, &value.start);
    put_path(&mut bytes[32..], &value.root);
    for (index, word) in [
        value.flags,
        value.max_symlinks,
        value.encoding,
        value.reserved,
    ]
    .into_iter()
    .enumerate()
    {
        put32(bytes, 64 + index * 4, word);
    }
}
pub fn open(bytes: &[u8]) -> VfsOpenRequest {
    VfsOpenRequest {
        path: path(bytes),
        access: u32_at(bytes, 32),
        share: u32_at(bytes, 36),
        stream_kind: u32_at(bytes, 40),
        encoding: u32_at(bytes, 44),
    }
}
pub fn put_open(bytes: &mut [u8], value: &VfsOpenRequest) {
    put_path(bytes, &value.path);
    for (index, word) in [value.access, value.share, value.stream_kind, value.encoding]
        .into_iter()
        .enumerate()
    {
        put32(bytes, 32 + index * 4, word);
    }
}
