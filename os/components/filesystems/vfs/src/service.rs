//! Per-consumer/Task reference tables. Core owns no paths, Nodes or files.
use crate::{
    Error, Result,
    file::OpenFile,
    namespace::{LookupContext, Namespace, Path},
    provider::{FsIdentity, NodeKind},
};
use alloc::vec::Vec;
use kcomp_sdk::{
    ipc::service::Request,
    vfs::{codec::*, *},
};
const LIMIT: usize = 32;
struct PathRef {
    token: VfsPath,
    consumer: u32,
    task: u32,
    refs: u32,
    path: Path,
}
struct FileRef {
    id: u64,
    consumer: u32,
    task: u32,
    refs: u32,
    file: OpenFile,
}
pub enum Undo {
    Path(VfsPath),
    File(u64),
    RetainFile(u64),
}
pub struct Service {
    namespace: Namespace,
    paths: Vec<PathRef>,
    files: Vec<FileRef>,
    filesystems: Vec<FsIdentity>,
    next_path: u64,
    next_file: u64,
}
impl Service {
    pub fn new(namespace: Namespace) -> Self {
        Self {
            namespace,
            paths: Vec::new(),
            files: Vec::new(),
            filesystems: Vec::new(),
            next_path: 1,
            next_file: 1,
        }
    }
    fn fs_id(&mut self, fs: FsIdentity) -> Result<u64> {
        if let Some(index) = self.filesystems.iter().position(|id| *id == fs) {
            return Ok(index as u64 + 1);
        }
        if self.filesystems.len() == LIMIT {
            return Err(Error::ENOSPC);
        }
        self.filesystems.push(fs);
        Ok(self.filesystems.len() as u64)
    }
    fn give_path(&mut self, consumer: u32, task: u32, path: Path) -> Result<VfsPath> {
        if let Some(reference) = self
            .paths
            .iter_mut()
            .find(|r| r.consumer == consumer && r.task == task && r.path.same_position(&path))
        {
            reference.refs = reference.refs.checked_add(1).ok_or(Error::EOVERFLOW)?;
            return Ok(reference.token);
        }
        if self.paths.len() == LIMIT {
            return Err(Error::ENOSPC);
        }
        let entry = self.next_path;
        let next = entry.checked_add(1).ok_or(Error::ENOSPC)?;
        let identity = path.node().identity();
        let token = VfsPath {
            mount: path.mount_id(),
            entry,
            fs: self.fs_id(identity.fs)?,
            node: identity.node,
        };
        self.next_path = next;
        self.paths.push(PathRef {
            token,
            consumer,
            task,
            refs: 1,
            path,
        });
        Ok(token)
    }
    fn path_index(&self, consumer: u32, task: u32, token: &VfsPath) -> Result<usize> {
        let index = self
            .paths
            .iter()
            .position(|r| r.token == *token)
            .ok_or(Error::ESTALE)?;
        let reference = &self.paths[index];
        if reference.consumer != consumer || reference.task != task {
            return Err(Error::EACCES);
        }
        Ok(index)
    }
    fn file_index(&self, consumer: u32, task: u32, id: u64) -> Result<usize> {
        let index = self
            .files
            .iter()
            .position(|r| r.id == id)
            .ok_or(Error::EBADF)?;
        let reference = &self.files[index];
        if reference.consumer != consumer || reference.task != task {
            return Err(Error::EACCES);
        }
        Ok(index)
    }
    fn release_path(&mut self, consumer: u32, task: u32, token: &VfsPath) -> Result<()> {
        let index = self.path_index(consumer, task, token)?;
        self.paths[index].refs -= 1;
        if self.paths[index].refs == 0 {
            self.paths.remove(index);
        }
        Ok(())
    }
    fn close(&mut self, consumer: u32, task: u32, id: u64) -> Result<()> {
        let index = self.file_index(consumer, task, id)?;
        self.files[index].refs -= 1;
        if self.files[index].refs != 0 {
            return Ok(());
        }
        let mut reference = self.files.remove(index);
        reference.file.close()
    }
    pub fn reap(&mut self, mut alive: impl FnMut(u32, u32) -> bool) {
        self.paths.retain(|r| alive(r.consumer, r.task));
        let mut index = 0;
        while index < self.files.len() {
            if alive(self.files[index].consumer, self.files[index].task) {
                index += 1;
            } else {
                let mut dead = self.files.remove(index);
                let _ = dead.file.close();
            }
        }
    }
    pub fn rollback(&mut self, consumer: u32, task: u32, undo: Undo) {
        match undo {
            Undo::Path(path) => {
                let _ = self.release_path(consumer, task, &path);
            }
            Undo::File(file) | Undo::RetainFile(file) => {
                let _ = self.close(consumer, task, file);
            }
        }
    }
    pub fn dispatch(
        &mut self,
        consumer: u32,
        task: u32,
        request: &Request<'_>,
        out: &mut [u8],
    ) -> Result<Option<Undo>> {
        // Validate complete shape before calling a backend or mutating a table.
        let expected = match request.method {
            KCOMP_VFS_METHOD_ROOT => (0, 40),
            KCOMP_VFS_METHOD_RESOLVE => (80, 40),
            KCOMP_VFS_METHOD_NODE_INFO => (32, 32),
            KCOMP_VFS_METHOD_OPEN => (48, 16),
            KCOMP_VFS_METHOD_RETAIN | KCOMP_VFS_METHOD_CLOSE => (8, 8),
            KCOMP_VFS_METHOD_SET_POSITION => (16, 8),
            KCOMP_VFS_METHOD_STREAM_INFO => (8, 64),
            KCOMP_VFS_METHOD_RETAIN_PATH | KCOMP_VFS_METHOD_RELEASE_PATH => (32, 8),
            KCOMP_VFS_METHOD_READ => (8, out.len()),
            KCOMP_VFS_METHOD_READ_AT => (16, out.len()),
            KCOMP_VFS_METHOD_READ_DIR => return Err(Error::ENOTSUP),
            _ => return Err(Error::ENOSYS),
        };
        if request.args.len() != expected.0
            || out.len() != expected.1
            || request.output != out.len()
            || out.len() < 8
            || ((request.method == KCOMP_VFS_METHOD_READ
                || request.method == KCOMP_VFS_METHOD_READ_AT)
                && !(16..=528).contains(&out.len()))
            || (!matches!(
                request.method,
                KCOMP_VFS_METHOD_RESOLVE | KCOMP_VFS_METHOD_OPEN
            ) && !request.input.is_empty())
        {
            return Err(Error::EINVAL);
        }
        out.fill(0);
        match request.method {
            KCOMP_VFS_METHOD_ROOT => {
                let token = self.give_path(consumer, task, self.namespace.root())?;
                put_path(&mut out[8..], &token);
                Ok(Some(Undo::Path(token)))
            }
            KCOMP_VFS_METHOD_RESOLVE => {
                let request_lookup = lookup(request.args);
                if request_lookup.reserved != 0 || request_lookup.flags & !7 != 0 {
                    return Err(Error::EINVAL);
                }
                if request_lookup.encoding != KCOMP_VFS_ENCODING_BYTES {
                    return Err(if request_lookup.encoding == KCOMP_VFS_ENCODING_UTF16 {
                        Error::ENOTSUP
                    } else {
                        Error::EINVAL
                    });
                }
                let start = self.paths[self.path_index(consumer, task, &request_lookup.start)?]
                    .path
                    .clone();
                let root = self.paths[self.path_index(consumer, task, &request_lookup.root)?]
                    .path
                    .clone();
                let found = self.namespace.resolve(
                    &LookupContext {
                        start: &start,
                        root: &root,
                        beneath: request_lookup.flags & KCOMP_VFS_LOOKUP_BENEATH_START != 0,
                        cross_mounts: request_lookup.flags & KCOMP_VFS_LOOKUP_CROSS_MOUNTS != 0,
                    },
                    request.input,
                )?;
                let token = self.give_path(consumer, task, found)?;
                put_path(&mut out[8..], &token);
                Ok(Some(Undo::Path(token)))
            }
            KCOMP_VFS_METHOD_RETAIN_PATH | KCOMP_VFS_METHOD_RELEASE_PATH => {
                let token = path(request.args);
                let index = self.path_index(consumer, task, &token)?;
                if request.method == KCOMP_VFS_METHOD_RELEASE_PATH {
                    self.release_path(consumer, task, &token)?;
                    Ok(None)
                } else {
                    self.paths[index].refs = self.paths[index]
                        .refs
                        .checked_add(1)
                        .ok_or(Error::EOVERFLOW)?;
                    Ok(Some(Undo::Path(token)))
                }
            }
            KCOMP_VFS_METHOD_NODE_INFO => {
                let token = path(request.args);
                let index = self.path_index(consumer, task, &token)?;
                let metadata = self.paths[index].path.node().metadata()?;
                put32(
                    out,
                    8,
                    if metadata.kind == NodeKind::Directory {
                        KCOMP_VFS_NODE_DIRECTORY
                    } else {
                        KCOMP_VFS_NODE_REGULAR
                    },
                );
                // No fabricated link count, permissions or directory case rules.
                Ok(None)
            }
            KCOMP_VFS_METHOD_OPEN => {
                let options = open(request.args);
                if options.access & !7 != 0 || options.share & !7 != 0 {
                    return Err(Error::EINVAL);
                }
                if options.access != KCOMP_VFS_ACCESS_READ
                    || options.share != KCOMP_VFS_SHARE_READ
                    || options.stream_kind != KCOMP_VFS_STREAM_DEFAULT
                {
                    return Err(Error::ENOTSUP);
                }
                if options.encoding != 0 || !request.input.is_empty() {
                    return Err(Error::EINVAL);
                }
                let index = self.path_index(consumer, task, &options.path)?;
                if self.files.len() == LIMIT {
                    return Err(Error::EMFILE);
                }
                let id = self.next_file;
                let next = id.checked_add(1).ok_or(Error::ENOSPC)?;
                let file = OpenFile::open(&self.paths[index].path)?;
                self.next_file = next;
                self.files.push(FileRef {
                    id,
                    consumer,
                    task,
                    refs: 1,
                    file,
                });
                put64(out, 8, id);
                Ok(Some(Undo::File(id)))
            }
            method => {
                let id = u64_at(request.args, 0);
                let index = self.file_index(consumer, task, id)?;
                match method {
                    KCOMP_VFS_METHOD_RETAIN => {
                        self.files[index].refs = self.files[index]
                            .refs
                            .checked_add(1)
                            .ok_or(Error::EOVERFLOW)?;
                        return Ok(Some(Undo::RetainFile(id)));
                    }
                    KCOMP_VFS_METHOD_CLOSE => {
                        self.close(consumer, task, id)?;
                    }
                    KCOMP_VFS_METHOD_SET_POSITION => {
                        self.files[index]
                            .file
                            .set_position(u64_at(request.args, 8))?;
                    }
                    KCOMP_VFS_METHOD_READ | KCOMP_VFS_METHOD_READ_AT => {
                        let actual = if method == KCOMP_VFS_METHOD_READ {
                            self.files[index].file.read(&mut out[16..])?
                        } else {
                            self.files[index]
                                .file
                                .read_at(u64_at(request.args, 8), &mut out[16..])?
                        };
                        put64(out, 8, actual as u64);
                    }
                    KCOMP_VFS_METHOD_STREAM_INFO => {
                        let node = self.files[index].file.node();
                        let identity = node.identity();
                        let metadata = node.metadata()?;
                        let fs = self
                            .filesystems
                            .iter()
                            .position(|fs| *fs == identity.fs)
                            .ok_or(Error::EIO)? as u64
                            + 1;
                        put64(out, 8, fs);
                        put64(out, 16, identity.node);
                        put64(out, 24, 1);
                        put64(out, 32, metadata.size);
                    }
                    _ => return Err(Error::ENOSYS),
                }
                Ok(None)
            }
        }
    }
}
