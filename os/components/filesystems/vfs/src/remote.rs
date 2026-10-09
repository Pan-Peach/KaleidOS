//! A borrowed mount-lifetime Node identity and an owned per-open provider lease.
//! Drop only queues a pre-reserved close; drain runs on the VFS Server Task.
use crate::{Error, Result, name::NameRef, provider::*};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use kcomp_sdk::generated::filesystem::*;
use kcomp_sdk::{
    endpoint::{Endpoint, InvokeError},
    filesystem::FileSystem as Contract,
    generated::filesystem_wire as wire,
};
use spin::Mutex;
const OPEN_LIMIT: usize = 8;
#[derive(Clone, Copy)]
enum Release {
    Free,
    Reserved,
    Pending(u64),
    Sending,
}
struct Connection {
    endpoint: u64,
    release: Mutex<[Release; OPEN_LIMIT]>,
}
pub struct RemoteFs {
    connection: Arc<Connection>,
    root: Node,
}
struct RemoteNode {
    connection: Arc<Connection>,
    id: u64,
}
struct RemoteOpen {
    connection: Arc<Connection>,
    handle: u64,
    slot: usize,
    closed: bool,
}
impl Connection {
    fn reserve(&self) -> Result<usize> {
        self.drain()?;
        let mut slots = self.release.lock();
        let index = slots
            .iter()
            .position(|r| matches!(r, Release::Free))
            .ok_or(Error::EMFILE)?;
        slots[index] = Release::Reserved;
        Ok(index)
    }
    fn defer(&self, slot: usize, handle: u64) {
        self.release.lock()[slot] = Release::Pending(handle);
    }
    fn drain(&self) -> Result<()> {
        loop {
            let pending = {
                let mut slots = self.release.lock();
                let found = slots.iter().enumerate().find_map(|(i, r)| match r {
                    Release::Pending(handle) => Some((i, *handle)),
                    _ => None,
                });
                if let Some((i, _)) = found {
                    slots[i] = Release::Sending;
                }
                found
            };
            let Some((index, handle)) = pending else {
                return Ok(());
            };
            let result = wire::close(self.endpoint, handle).map_err(error);
            // Transport backpressure has not submitted a close; keep ownership.
            // Method/endpoint failure consumes or invalidates the provider lease.
            let retry = matches!(result, Err(Error::ENOBUFS | Error::EBUSY | Error::EAGAIN));
            self.release.lock()[index] = if retry {
                Release::Pending(handle)
            } else {
                Release::Free
            };
            if retry {
                return result;
            }
        }
    }
}
impl RemoteFs {
    pub fn connect(endpoint: u64) -> Result<Self> {
        Endpoint::<Contract>::from_id(endpoint)?;
        let connection = Arc::new(Connection {
            endpoint,
            release: Mutex::new([Release::Free; OPEN_LIMIT]),
        });
        wire::mount(endpoint).map_err(error)?;
        let id = wire::root(endpoint).map_err(error)?;
        if id == 0 {
            return Err(Error::EPROTO);
        }
        Ok(Self {
            root: Arc::new(RemoteNode {
                connection: connection.clone(),
                id,
            }),
            connection,
        })
    }
    pub fn drain(&self) -> Result<()> {
        self.connection.drain()
    }
}
impl FileSystem for RemoteFs {
    fn root(&self) -> Result<Node> {
        Ok(self.root.clone())
    }
}
impl RemoteNode {
    fn details(&self) -> Result<(Metadata, Vec<u8>)> {
        let mut name = [0; 12];
        let details =
            wire::node_details(self.connection.endpoint, self.id, &mut name).map_err(error)?;
        let kind = match details.kind {
            KCOMP_FILESYSTEM_NODE_FILE => NodeKind::File,
            KCOMP_FILESYSTEM_NODE_DIRECTORY => NodeKind::Directory,
            _ => return Err(Error::EPROTO),
        };
        let len = details.name_length as usize;
        if len > 12 {
            return Err(Error::EPROTO);
        }
        Ok((
            Metadata {
                kind,
                size: details.size,
            },
            name[..len].to_vec(),
        ))
    }
}
impl FsNode for RemoteNode {
    fn identity(&self) -> NodeIdentity {
        NodeIdentity {
            fs: FsIdentity {
                provider: self.connection.endpoint,
                instance: self.connection.endpoint,
            },
            node: self.id,
        }
    }
    fn metadata(&self) -> Result<Metadata> {
        self.details().map(|(metadata, _)| metadata)
    }
    fn lookup(&self, name: NameRef<'_>) -> Result<Lookup> {
        let NameRef::Bytes(name) = name else {
            return Err(Error::ENOTSUP);
        };
        crate::name::check_component(name)?;
        let id = wire::lookup(
            self.connection.endpoint,
            self.id,
            KCOMP_FILESYSTEM_ENCODING_BYTES,
            name,
        )
        .map_err(error)?;
        if id == 0 {
            return Err(Error::EPROTO);
        }
        let node = RemoteNode {
            connection: self.connection.clone(),
            id,
        };
        let (_, canonical) = node.details()?;
        crate::name::check_component(&canonical)?;
        Ok(Lookup {
            name: canonical,
            node: Arc::new(node),
        })
    }
    fn open(&self) -> Result<Box<dyn FsOpen>> {
        let index = self.connection.reserve()?;
        let handle = match wire::open_node(self.connection.endpoint, self.id).map_err(error) {
            Ok(handle) => handle,
            Err(error) => {
                self.connection.release.lock()[index] = Release::Free;
                return Err(error);
            }
        };
        if handle == 0 {
            self.connection.release.lock()[index] = Release::Free;
            return Err(Error::EPROTO);
        }
        Ok(Box::new(RemoteOpen {
            connection: self.connection.clone(),
            handle,
            slot: index,
            closed: false,
        }))
    }
}
impl FsOpen for RemoteOpen {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<usize> {
        if self.closed {
            return Err(Error::EBADF);
        }
        let count = out.len().min(512);
        let mut bytes = [0; 512];
        let actual = wire::read_at(
            self.connection.endpoint,
            self.handle,
            offset,
            &mut bytes[..count],
        )
        .map_err(error)?;
        if actual > count as u64 {
            return Err(Error::EPROTO);
        }
        let actual = actual as usize;
        out[..actual].copy_from_slice(&bytes[..actual]);
        Ok(actual)
    }
    fn close(&mut self) -> Result<()> {
        if self.closed {
            return Err(Error::EBADF);
        }
        self.closed = true;
        self.connection.defer(self.slot, self.handle);
        self.connection.drain()
    }
}
impl Drop for RemoteOpen {
    fn drop(&mut self) {
        if !self.closed {
            self.connection.defer(self.slot, self.handle);
        }
    }
}

fn error(error: InvokeError) -> Error {
    match error {
        InvokeError::Method(errno) | InvokeError::Transport(errno) => errno,
        InvokeError::InvalidReply => Error::EPROTO,
    }
}
