//! A single OpenFile semantics for every backend; user handles belong to VFS.
use crate::{
    Error, Result,
    namespace::Path,
    provider::{FsOpen, Node},
};
use alloc::boxed::Box;
pub struct OpenFile {
    path: Path,
    backend: Box<dyn FsOpen>,
    position: u64,
    closed: bool,
}
impl OpenFile {
    pub fn open(path: &Path) -> Result<Self> {
        let node = path.node().clone();
        let backend = node.open()?;
        Ok(Self {
            path: path.clone(),
            backend,
            position: 0,
            closed: false,
        })
    }
    pub fn node(&self) -> &Node {
        self.path.node()
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<usize> {
        if self.closed {
            return Err(Error::EBADF);
        }
        let length = self.backend.read_at(offset, out)?;
        if length > out.len() {
            return Err(Error::EIO);
        }
        Ok(length)
    }
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        if self.closed {
            return Err(Error::EBADF);
        }
        if (out.len() as u64).checked_add(self.position).is_none() {
            return Err(Error::EOVERFLOW);
        }
        let length = self.read_at(self.position, out)?;
        self.position += length as u64;
        Ok(length)
    }
    pub fn set_position(&mut self, position: u64) -> Result<()> {
        if self.closed {
            return Err(Error::EBADF);
        }
        self.position = position;
        Ok(())
    }
    pub fn close(&mut self) -> Result<()> {
        if self.closed {
            return Err(Error::EBADF);
        }
        self.closed = true;
        // Consumed even when backend close fails; never retry a consumed lease.
        self.backend.close()
    }
}
