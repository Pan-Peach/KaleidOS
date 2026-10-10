//! Per-consumer/Task reference tables. Core owns no paths, Nodes or files.
use crate::{
    Error, Result,
    file::OpenFile,
    namespace::{LookupContext, Namespace, Path},
    provider::{FsIdentity, NodeKind},
};
use alloc::vec::Vec;
use kcomp_sdk::{generated::vfs_wire as wire, ipc::service::Request, vfs::*};
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
        out.fill(0);
        let mut handler = Handler {
            service: self,
            consumer,
            task,
            undo: None,
        };
        let status = wire::dispatch(&mut handler, request, out);
        if status != 0 {
            return Err(Error::from_code(status));
        }
        Ok(handler.undo)
    }
}

// Verified caller identity and cancellation compensation stay local to VFS.
struct Handler<'a> {
    service: &'a mut Service,
    consumer: u32,
    task: u32,
    undo: Option<Undo>,
}
fn reply_status() -> VfsReplyStatus {
    VfsReplyStatus {
        domain: 0,
        reserved: 0,
    }
}
impl wire::Provider for Handler<'_> {
    fn root(&mut self) -> Result<wire::RootReply> {
        let token =
            self.service
                .give_path(self.consumer, self.task, self.service.namespace.root())?;
        self.undo = Some(Undo::Path(token));
        Ok(wire::RootReply {
            reply_status: reply_status(),
            token,
        })
    }
    fn resolve(&mut self, options: VfsLookup, input: &[u8]) -> Result<wire::ResolveReply> {
        if options.reserved != 0 || options.flags & !7 != 0 {
            return Err(Error::EINVAL);
        }
        if options.encoding != KCOMP_VFS_ENCODING_BYTES {
            return Err(if options.encoding == KCOMP_VFS_ENCODING_UTF16 {
                Error::ENOTSUP
            } else {
                Error::EINVAL
            });
        }
        let start = self.service.paths[self.service.path_index(
            self.consumer,
            self.task,
            &options.start,
        )?]
        .path
        .clone();
        let root =
            self.service.paths[self
                .service
                .path_index(self.consumer, self.task, &options.root)?]
            .path
            .clone();
        let found = self.service.namespace.resolve(
            &LookupContext {
                start: &start,
                root: &root,
                beneath: options.flags & KCOMP_VFS_LOOKUP_BENEATH_START != 0,
                cross_mounts: options.flags & KCOMP_VFS_LOOKUP_CROSS_MOUNTS != 0,
            },
            input,
        )?;
        let token = self.service.give_path(self.consumer, self.task, found)?;
        self.undo = Some(Undo::Path(token));
        Ok(wire::ResolveReply {
            reply_status: reply_status(),
            token,
        })
    }
    fn retain_path(&mut self, token: VfsPath) -> Result<VfsReplyStatus> {
        let index = self.service.path_index(self.consumer, self.task, &token)?;
        self.service.paths[index].refs = self.service.paths[index]
            .refs
            .checked_add(1)
            .ok_or(Error::EOVERFLOW)?;
        self.undo = Some(Undo::Path(token));
        Ok(reply_status())
    }
    fn release_path(&mut self, token: VfsPath) -> Result<VfsReplyStatus> {
        self.service
            .release_path(self.consumer, self.task, &token)?;
        Ok(reply_status())
    }
    fn node_info(&mut self, token: VfsPath) -> Result<wire::NodeInfoReply> {
        let index = self.service.path_index(self.consumer, self.task, &token)?;
        let metadata = self.service.paths[index].path.node().metadata()?;
        // Unknown metadata remains explicitly invalid, not fabricated.
        let info = VfsNodeInfo {
            kind: if metadata.kind == NodeKind::Directory {
                KCOMP_VFS_NODE_DIRECTORY
            } else {
                KCOMP_VFS_NODE_REGULAR
            },
            valid: 0,
            link_count: 0,
            name_encoding: 0,
            case_rule: 0,
        };
        Ok(wire::NodeInfoReply {
            reply_status: reply_status(),
            info,
        })
    }
    fn open(&mut self, options: VfsOpenRequest, input: &[u8]) -> Result<wire::OpenReply> {
        if options.access & !7 != 0 || options.share & !7 != 0 {
            return Err(Error::EINVAL);
        }
        if options.access != KCOMP_VFS_ACCESS_READ
            || options.share != KCOMP_VFS_SHARE_READ
            || options.stream_kind != KCOMP_VFS_STREAM_DEFAULT
        {
            return Err(Error::ENOTSUP);
        }
        if options.encoding != 0 || !input.is_empty() {
            return Err(Error::EINVAL);
        }
        let index = self
            .service
            .path_index(self.consumer, self.task, &options.path)?;
        if self.service.files.len() == LIMIT {
            return Err(Error::EMFILE);
        }
        let id = self.service.next_file;
        let next = id.checked_add(1).ok_or(Error::ENOSPC)?;
        let file = OpenFile::open(&self.service.paths[index].path)?;
        self.service.next_file = next;
        self.service.files.push(FileRef {
            id,
            consumer: self.consumer,
            task: self.task,
            refs: 1,
            file,
        });
        self.undo = Some(Undo::File(id));
        Ok(wire::OpenReply {
            reply_status: reply_status(),
            file: id,
        })
    }
    fn retain(&mut self, file: u64) -> Result<VfsReplyStatus> {
        let index = self.service.file_index(self.consumer, self.task, file)?;
        self.service.files[index].refs = self.service.files[index]
            .refs
            .checked_add(1)
            .ok_or(Error::EOVERFLOW)?;
        self.undo = Some(Undo::RetainFile(file));
        Ok(reply_status())
    }
    fn close(&mut self, file: u64) -> Result<VfsReplyStatus> {
        self.service.close(self.consumer, self.task, file)?;
        Ok(reply_status())
    }
    fn set_position(&mut self, file: u64, offset: u64) -> Result<VfsReplyStatus> {
        let index = self.service.file_index(self.consumer, self.task, file)?;
        self.service.files[index].file.set_position(offset)?;
        Ok(reply_status())
    }
    fn read(&mut self, file: u64, output: &mut [u8]) -> Result<wire::ReadReply> {
        let index = self.service.file_index(self.consumer, self.task, file)?;
        let actual = self.service.files[index].file.read(output)? as u64;
        Ok(wire::ReadReply {
            reply_status: reply_status(),
            actual,
        })
    }
    fn read_at(&mut self, file: u64, offset: u64, output: &mut [u8]) -> Result<wire::ReadAtReply> {
        let index = self.service.file_index(self.consumer, self.task, file)?;
        let actual = self.service.files[index].file.read_at(offset, output)? as u64;
        Ok(wire::ReadAtReply {
            reply_status: reply_status(),
            actual,
        })
    }
    fn stream_info(&mut self, file: u64) -> Result<wire::StreamInfoReply> {
        let index = self.service.file_index(self.consumer, self.task, file)?;
        let node = self.service.files[index].file.node();
        let identity = node.identity();
        let metadata = node.metadata()?;
        let fs = self
            .service
            .filesystems
            .iter()
            .position(|fs| *fs == identity.fs)
            .ok_or(Error::EIO)? as u64
            + 1;
        let info = VfsStreamInfo {
            stream: VfsStream {
                fs,
                node: identity.node,
                stream: 1,
            },
            size: metadata.size,
            allocated_size: 0,
            valid_data_length: 0,
            valid: 0,
            reserved: 0,
        };
        Ok(wire::StreamInfoReply {
            reply_status: reply_status(),
            info,
        })
    }
    fn read_dir(&mut self, _: VfsPath, _: u64, _: &mut [u8]) -> Result<wire::ReadDirReply> {
        Err(Error::ENOTSUP)
    }
    fn shutdown(&mut self) -> Result<VfsReplyStatus> {
        // Only runtime's verified control branch may stop the service.
        Err(Error::EACCES)
    }
}
