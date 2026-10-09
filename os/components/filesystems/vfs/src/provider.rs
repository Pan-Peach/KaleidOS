//! Object-safe FS semantics within one VFS image; Rust objects never cross ABI.
use crate::{Result, name::NameRef};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsIdentity {
    pub provider: u64,
    pub instance: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeIdentity {
    pub fs: FsIdentity,
    pub node: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub kind: NodeKind,
    pub size: u64,
}
pub type Node = Arc<dyn FsNode>;
pub struct Lookup {
    /// Canonical single-component name selected by the backend's matching rules.
    pub name: Vec<u8>,
    pub node: Node,
}
pub trait FileSystem: Send + Sync {
    fn root(&self) -> Result<Node>;
}
pub trait FsNode: Send + Sync {
    fn identity(&self) -> NodeIdentity;
    fn metadata(&self) -> Result<Metadata>;
    /// Returned node belongs to this FS instance; namespace crossing uses mounts.
    fn lookup(&self, name: NameRef<'_>) -> Result<Lookup>;
    fn open(&self) -> Result<Box<dyn FsOpen>>;
}
/// One backend open object. VFS owns the exposed cursor; offsets are explicit.
pub trait FsOpen: Send {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<usize>;
    /// Consume the backend lease once, including failure. Drop must release any
    /// remaining local ownership; future remote backends must defer IPC release.
    fn close(&mut self) -> Result<()>;
}
