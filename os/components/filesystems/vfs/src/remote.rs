//! A borrowed mount-lifetime Node identity and an owned per-open provider lease.
//! Drop only queues a pre-reserved close; drain runs on the VFS Server Task.
use crate::{Error, Result, name::NameRef, provider::*};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use kcomp_sdk::generated::filesystem::*;
use kcomp_sdk::{endpoint::Endpoint, filesystem::FileSystem as Contract, ipc};
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
    fn invoke(&self, method: u32, args: &[u8], input: &[u8], output: &mut [u8]) -> Result<()> {
        let code = ipc::service::invoke(self.endpoint, method, args, input, output)?;
        if code == 0 {
            Ok(())
        } else {
            Err(Error::from_code(code))
        }
    }
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
            let result = self.invoke(
                KCOMP_FILESYSTEM_METHOD_CLOSE,
                &handle.to_le_bytes(),
                &[],
                &mut [],
            );
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
        connection.invoke(KCOMP_FILESYSTEM_METHOD_MOUNT, &[], &[], &mut [])?;
        let mut bytes = [0; 8];
        connection.invoke(KCOMP_FILESYSTEM_METHOD_ROOT, &[], &[], &mut bytes)?;
        let id = u64::from_le_bytes(bytes);
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
        let mut bytes = [0; 28];
        self.connection.invoke(
            KCOMP_FILESYSTEM_METHOD_NODE_DETAILS,
            &self.id.to_le_bytes(),
            &[],
            &mut bytes,
        )?;
        let kind = match u32::from_le_bytes(bytes[..4].try_into().unwrap()) {
            KCOMP_FILESYSTEM_NODE_FILE => NodeKind::File,
            KCOMP_FILESYSTEM_NODE_DIRECTORY => NodeKind::Directory,
            _ => return Err(Error::EPROTO),
        };
        let len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        if len > 12 {
            return Err(Error::EPROTO);
        }
        Ok((
            Metadata {
                kind,
                size: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            },
            bytes[16..16 + len].to_vec(),
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
        let mut args = [0; 12];
        args[..8].copy_from_slice(&self.id.to_le_bytes());
        args[8..].copy_from_slice(&KCOMP_FILESYSTEM_ENCODING_BYTES.to_le_bytes());
        let mut bytes = [0; 8];
        self.connection
            .invoke(KCOMP_FILESYSTEM_METHOD_LOOKUP, &args, name, &mut bytes)?;
        let id = u64::from_le_bytes(bytes);
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
        let mut bytes = [0; 8];
        let result = self.connection.invoke(
            KCOMP_FILESYSTEM_METHOD_OPEN_NODE,
            &self.id.to_le_bytes(),
            &[],
            &mut bytes,
        );
        if let Err(error) = result {
            self.connection.release.lock()[index] = Release::Free;
            return Err(error);
        }
        let handle = u64::from_le_bytes(bytes);
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
        let mut args = [0; 16];
        args[..8].copy_from_slice(&self.handle.to_le_bytes());
        args[8..].copy_from_slice(&offset.to_le_bytes());
        let mut bytes = [0; 520];
        self.connection.invoke(
            KCOMP_FILESYSTEM_METHOD_READ_AT,
            &args,
            &[],
            &mut bytes[..8 + count],
        )?;
        let actual = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        if actual > count as u64 {
            return Err(Error::EPROTO);
        }
        let actual = actual as usize;
        out[..actual].copy_from_slice(&bytes[8..8 + actual]);
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
