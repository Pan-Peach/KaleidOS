//! One namespace for Local and Remote Nodes. Mounts attach to dentry positions.
use crate::{
    Error, Result,
    name::NameRef,
    provider::{Node, NodeKind},
};
use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use spin::Mutex;
struct Dentry {
    node: Node,
    name: Vec<u8>,
    parent: Option<Arc<Dentry>>,
    children: Mutex<Vec<Weak<Dentry>>>,
}
impl Dentry {
    fn root(node: Node) -> Arc<Self> {
        Arc::new(Self {
            node,
            name: Vec::new(),
            parent: None,
            children: Mutex::new(Vec::new()),
        })
    }
    fn lookup(self: &Arc<Self>, name: &[u8]) -> Result<Arc<Self>> {
        // No namespace or cache lock across a backend call (which may park).
        let found = self.node.lookup(NameRef::Bytes(name))?;
        crate::name::check_component(&found.name)?;
        if found.node.identity().fs != self.node.identity().fs {
            return Err(Error::EIO);
        }
        let mut cache = self.children.lock();
        cache.retain(|entry| entry.strong_count() != 0);
        if let Some(entry) = cache.iter().filter_map(Weak::upgrade).find(|entry| {
            entry.name == found.name && entry.node.identity() == found.node.identity()
        }) {
            return Ok(entry);
        }
        let entry = Arc::new(Self {
            node: found.node,
            name: found.name,
            parent: Some(self.clone()),
            children: Mutex::new(Vec::new()),
        });
        cache.push(Arc::downgrade(&entry));
        Ok(entry)
    }
}
struct Mount {
    id: u64,
    root: Arc<Dentry>,
    covered: Option<Path>,
}
#[derive(Clone)]
pub struct Path {
    mount: Arc<Mount>,
    entry: Arc<Dentry>,
}
impl Path {
    pub fn node(&self) -> &Node {
        &self.entry.node
    }
    pub fn mount_id(&self) -> u64 {
        self.mount.id
    }
    pub fn same_position(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mount, &other.mount) && Arc::ptr_eq(&self.entry, &other.entry)
    }
    fn parent(&self) -> Self {
        if let Some(parent) = &self.entry.parent {
            return Self {
                mount: self.mount.clone(),
                entry: parent.clone(),
            };
        }
        if let Some(covered) = &self.mount.covered {
            return covered.parent();
        }
        self.clone()
    }
    fn below(&self, root: &Self) -> bool {
        let mut path = self.clone();
        loop {
            if path.same_position(root) {
                return true;
            }
            let parent = path.parent();
            if parent.same_position(&path) {
                return false;
            }
            path = parent;
        }
    }
}
pub struct LookupContext<'a> {
    pub start: &'a Path,
    pub root: &'a Path,
    pub beneath: bool,
    pub cross_mounts: bool,
}
pub struct Namespace {
    root: Path,
    mounts: Vec<Arc<Mount>>,
    next_mount: u64,
}
impl Namespace {
    pub fn new(root: Node) -> Result<Self> {
        if root.metadata()?.kind != NodeKind::Directory {
            return Err(Error::ENOTDIR);
        }
        let mount = Arc::new(Mount {
            id: 1,
            root: Dentry::root(root),
            covered: None,
        });
        Ok(Self {
            root: Path {
                mount: mount.clone(),
                entry: mount.root.clone(),
            },
            mounts: alloc::vec![mount],
            next_mount: 2,
        })
    }
    pub fn root(&self) -> Path {
        self.root.clone()
    }
    fn contains(&self, path: &Path) -> bool {
        self.mounts
            .iter()
            .any(|mount| Arc::ptr_eq(mount, &path.mount))
    }
    pub fn attach(&mut self, at: &Path, root: Node) -> Result<u64> {
        if !self.contains(at) {
            return Err(Error::ESTALE);
        }
        if at.same_position(&self.root) {
            return Err(Error::ENOTSUP);
        }
        if at.node().metadata()?.kind != NodeKind::Directory
            || root.metadata()?.kind != NodeKind::Directory
        {
            return Err(Error::ENOTDIR);
        }
        if self.mounts.iter().any(|mount| {
            mount
                .covered
                .as_ref()
                .is_some_and(|path| path.same_position(at))
        }) {
            return Err(Error::EBUSY);
        }
        let id = self.next_mount;
        self.next_mount = id.checked_add(1).ok_or(Error::ENOSPC)?;
        self.mounts.push(Arc::new(Mount {
            id,
            root: Dentry::root(root),
            covered: Some(at.clone()),
        }));
        Ok(id)
    }
    pub fn detach(&mut self, id: u64) -> Result<()> {
        let index = self
            .mounts
            .iter()
            .position(|mount| mount.id == id)
            .ok_or(Error::ENOENT)?;
        if index == 0 || Arc::strong_count(&self.mounts[index]) != 1 {
            return Err(Error::EBUSY);
        }
        self.mounts.remove(index);
        Ok(())
    }
    pub fn resolve(&self, ctx: &LookupContext<'_>, path: &[u8]) -> Result<Path> {
        if !self.contains(ctx.start) || !self.contains(ctx.root) {
            return Err(Error::ESTALE);
        }
        if !ctx.start.below(ctx.root) {
            return Err(Error::EXDEV);
        }
        if path.is_empty() || path.contains(&0) || path.len() > 512 {
            return Err(Error::EINVAL);
        }
        let absolute = path.starts_with(b"/");
        if ctx.beneath && absolute {
            return Err(Error::EXDEV);
        }
        let mut current = if absolute {
            ctx.root.clone()
        } else {
            ctx.start.clone()
        };
        for name in path.split(|b| *b == b'/').filter(|part| !part.is_empty()) {
            if current.node().metadata()?.kind != NodeKind::Directory {
                return Err(Error::ENOTDIR);
            }
            if name == b"." {
                continue;
            }
            if name == b".." {
                if ctx.beneath && current.same_position(ctx.start) {
                    return Err(Error::EXDEV);
                }
                if !current.same_position(ctx.root) {
                    let parent = current.parent();
                    if !ctx.cross_mounts && !Arc::ptr_eq(&parent.mount, &current.mount) {
                        return Err(Error::EXDEV);
                    }
                    current = parent;
                }
                continue;
            }
            crate::name::check_component(name)?;
            current.entry = current.entry.lookup(name)?;
            if let Some(mount) = self.mounts.iter().find(|mount| {
                mount
                    .covered
                    .as_ref()
                    .is_some_and(|path| path.same_position(&current))
            }) {
                if !ctx.cross_mounts {
                    return Err(Error::EXDEV);
                }
                current = Path {
                    mount: mount.clone(),
                    entry: mount.root.clone(),
                };
            }
        }
        if path.ends_with(b"/") && current.node().metadata()?.kind != NodeKind::Directory {
            return Err(Error::ENOTDIR);
        }
        Ok(current)
    }
}
