//! Minimal read-only memory FS; plain records shared by Arc, no object registry.
use crate::{Error, Result, name::NameRef, provider::*};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};
static NEXT_INSTANCE: AtomicU32 = AtomicU32::new(1);
/// Records are topologically ordered: parent 0 is the implicit root directory.
pub struct LocalEntry<'a> {
    pub parent: usize,
    pub name: &'a [u8],
    pub data: Option<&'a [u8]>,
}
struct Record {
    parent: usize,
    name: Vec<u8>,
    data: Option<Arc<[u8]>>,
}
struct Tree {
    identity: FsIdentity,
    records: Vec<Record>,
}
pub struct LocalFs {
    tree: Arc<Tree>,
}
struct LocalNode {
    tree: Arc<Tree>,
    index: usize,
}
struct LocalOpen {
    data: Arc<[u8]>,
    closed: bool,
}
impl LocalFs {
    pub fn new(entries: &[LocalEntry<'_>]) -> Result<Self> {
        let mut records = Vec::new();
        records.push(Record {
            parent: 0,
            name: Vec::new(),
            data: None,
        });
        for entry in entries {
            crate::name::check_component(entry.name)?;
            if entry.parent >= records.len() {
                return Err(Error::EINVAL);
            }
            if records[entry.parent].data.is_some() {
                return Err(Error::ENOTDIR);
            }
            if records
                .iter()
                .any(|r| r.parent == entry.parent && r.name == entry.name)
            {
                return Err(Error::EEXIST);
            }
            records.push(Record {
                parent: entry.parent,
                name: entry.name.to_vec(),
                data: entry.data.map(Arc::from),
            });
        }
        let id = NEXT_INSTANCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| Error::ENOSPC)?;
        Ok(Self {
            tree: Arc::new(Tree {
                identity: FsIdentity {
                    provider: 0,
                    instance: u64::from(id),
                },
                records,
            }),
        })
    }
}
impl FileSystem for LocalFs {
    fn root(&self) -> Result<Node> {
        Ok(Arc::new(LocalNode {
            tree: self.tree.clone(),
            index: 0,
        }))
    }
}
impl FsNode for LocalNode {
    fn identity(&self) -> NodeIdentity {
        NodeIdentity {
            fs: self.tree.identity,
            node: self.index as u64 + 1,
        }
    }
    fn metadata(&self) -> Result<Metadata> {
        Ok(match &self.tree.records[self.index].data {
            Some(data) => Metadata {
                kind: NodeKind::File,
                size: data.len() as u64,
            },
            None => Metadata {
                kind: NodeKind::Directory,
                size: 0,
            },
        })
    }
    fn lookup(&self, name: NameRef<'_>) -> Result<Lookup> {
        if self.tree.records[self.index].data.is_some() {
            return Err(Error::ENOTDIR);
        }
        let NameRef::Bytes(name) = name else {
            return Err(Error::ENOTSUP);
        };
        crate::name::check_component(name)?;
        let index = self
            .tree
            .records
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, r)| r.parent == self.index && r.name == name)
            .map(|(i, _)| i)
            .ok_or(Error::ENOENT)?;
        Ok(Lookup {
            name: self.tree.records[index].name.clone(),
            node: Arc::new(Self {
                tree: self.tree.clone(),
                index,
            }),
        })
    }
    fn open(&self) -> Result<Box<dyn FsOpen>> {
        let data = self.tree.records[self.index]
            .data
            .as_ref()
            .ok_or(Error::EISDIR)?;
        Ok(Box::new(LocalOpen {
            data: data.clone(),
            closed: false,
        }))
    }
}
impl FsOpen for LocalOpen {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<usize> {
        if self.closed {
            return Err(Error::EBADF);
        }
        let start = offset.min(self.data.len() as u64) as usize;
        let len = out.len().min(self.data.len() - start);
        out[..len].copy_from_slice(&self.data[start..start + len]);
        Ok(len)
    }
    fn close(&mut self) -> Result<()> {
        if self.closed {
            return Err(Error::EBADF);
        }
        self.closed = true;
        Ok(())
    }
}
