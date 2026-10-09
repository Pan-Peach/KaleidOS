use crate::{
    Error,
    file::OpenFile,
    local::{LocalEntry, LocalFs},
    name::NameRef,
    namespace::{LookupContext, Namespace, Path},
    provider::*,
};
use alloc::{boxed::Box, sync::Arc};
fn fs() -> LocalFs {
    LocalFs::new(&[
        LocalEntry {
            parent: 0,
            name: b"dir",
            data: None,
        },
        LocalEntry {
            parent: 1,
            name: b"file",
            data: Some(b"hello"),
        },
        LocalEntry {
            parent: 0,
            name: b"mount",
            data: None,
        },
        LocalEntry {
            parent: 0,
            name: b"other",
            data: None,
        },
    ])
    .unwrap()
}
fn resolve(ns: &Namespace, start: &Path, path: &[u8]) -> crate::Result<Path> {
    ns.resolve(
        &LookupContext {
            start,
            root: &ns.root(),
            beneath: false,
            cross_mounts: true,
        },
        path,
    )
}
#[test]
fn real_local_objects_nested_lookup_cursors_and_metadata() {
    let fs = fs();
    let ns = Namespace::new(fs.root().unwrap()).unwrap();
    let root = ns.root();
    let a = resolve(&ns, &root, b"dir/file").unwrap();
    let b = resolve(&ns, &root, b"/dir/file").unwrap();
    assert!(a.same_position(&b));
    assert_eq!(a.node().identity(), b.node().identity());
    assert_eq!(
        a.node().metadata().unwrap(),
        Metadata {
            kind: NodeKind::File,
            size: 5
        }
    );
    let mut first = OpenFile::open(&a).unwrap();
    let mut second = OpenFile::open(&b).unwrap();
    drop(a);
    drop(b);
    drop(fs);
    let mut bytes = [0; 8];
    assert_eq!(first.read(&mut bytes[..2]), Ok(2));
    assert_eq!(&bytes[..2], b"he");
    assert_eq!(first.read_at(0, &mut bytes), Ok(5));
    assert_eq!(first.position(), 2);
    assert_eq!(first.read(&mut bytes), Ok(3));
    assert_eq!(&bytes[..3], b"llo");
    assert_eq!(first.read(&mut bytes), Ok(0));
    assert_eq!(first.read(&mut []), Ok(0));
    assert_eq!(second.read(&mut bytes), Ok(5));
    assert_eq!(&bytes[..5], b"hello");
    assert_eq!(first.close(), Ok(()));
    assert_eq!(first.close(), Err(Error::EBADF));
    assert_eq!(first.read_at(0, &mut []), Err(Error::EBADF));
}
#[test]
fn native_instances_mount_positions_parent_and_unmount_references() {
    let mut ns = Namespace::new(fs().root().unwrap()).unwrap();
    let root = ns.root();
    let a = resolve(&ns, &root, b"mount").unwrap();
    let b = resolve(&ns, &root, b"other").unwrap();
    let one = fs();
    let two = fs();
    assert_ne!(
        one.root().unwrap().identity(),
        two.root().unwrap().identity()
    );
    let id = ns.attach(&a, one.root().unwrap()).unwrap();
    ns.attach(&b, two.root().unwrap()).unwrap();
    let first = resolve(&ns, &root, b"mount/dir/file").unwrap();
    let second = resolve(&ns, &root, b"other/dir/file").unwrap();
    assert_ne!(first.node().identity(), second.node().identity());
    assert!(
        resolve(&ns, &root, b"mount/..")
            .unwrap()
            .same_position(&root)
    );
    let opened = OpenFile::open(&first).unwrap();
    drop(first);
    assert_eq!(ns.detach(id), Err(Error::EBUSY));
    drop(opened);
    assert_eq!(ns.detach(id), Ok(()));
    assert!(matches!(
        resolve(&ns, &root, b"mount/dir"),
        Err(Error::ENOENT)
    ));
}
#[test]
fn constrained_walk_errors_and_explicit_no_cross() {
    let mut ns = Namespace::new(fs().root().unwrap()).unwrap();
    let root = ns.root();
    let dir = resolve(&ns, &root, b"dir").unwrap();
    let context = LookupContext {
        start: &dir,
        root: &root,
        beneath: true,
        cross_mounts: true,
    };
    assert!(matches!(ns.resolve(&context, b"../dir"), Err(Error::EXDEV)));
    assert!(matches!(ns.resolve(&context, b"/dir"), Err(Error::EXDEV)));
    assert!(matches!(
        ns.resolve(&context, b"file/"),
        Err(Error::ENOTDIR)
    ));
    assert!(matches!(
        ns.resolve(&context, b"file/."),
        Err(Error::ENOTDIR)
    ));
    assert!(matches!(
        ns.resolve(&context, b"missing"),
        Err(Error::ENOENT)
    ));
    assert!(matches!(
        ns.resolve(&context, b"file/child"),
        Err(Error::ENOTDIR)
    ));
    assert!(matches!(
        ns.resolve(&context, b"file\0bad"),
        Err(Error::EINVAL)
    ));
    let restricted = LookupContext {
        start: &dir,
        root: &dir,
        beneath: false,
        cross_mounts: true,
    };
    assert!(
        ns.resolve(&restricted, b"../../")
            .unwrap()
            .same_position(&dir)
    );
    let at = resolve(&ns, &root, b"mount").unwrap();
    ns.attach(&at, fs().root().unwrap()).unwrap();
    let context = LookupContext {
        start: &root,
        root: &root,
        beneath: false,
        cross_mounts: false,
    };
    assert!(matches!(
        ns.resolve(&context, b"mount/dir"),
        Err(Error::EXDEV)
    ));
    let foreign = Namespace::new(fs().root().unwrap()).unwrap();
    let context = LookupContext {
        start: &foreign.root(),
        ..context
    };
    assert!(matches!(ns.resolve(&context, b"dir"), Err(Error::ESTALE)));
}
struct Alias {
    inner: Node,
}
impl FsNode for Alias {
    fn identity(&self) -> NodeIdentity {
        self.inner.identity()
    }
    fn metadata(&self) -> crate::Result<Metadata> {
        self.inner.metadata()
    }
    fn lookup(&self, name: NameRef<'_>) -> crate::Result<Lookup> {
        let NameRef::Bytes(bytes) = name else {
            return Err(Error::ENOTSUP);
        };
        let mut found = self.inner.lookup(NameRef::Bytes(if bytes == b"alias" {
            b"dir"
        } else {
            bytes
        }))?;
        found.name = bytes.to_vec();
        Ok(found)
    }
    fn open(&self) -> crate::Result<Box<dyn FsOpen>> {
        self.inner.open()
    }
}
#[test]
fn alias_entries_do_not_merge_mounts_by_node_identity() {
    // A test backend adds a second name for the same production Local directory.
    let node: Node = Arc::new(Alias {
        inner: fs().root().unwrap(),
    });
    let mut ns = Namespace::new(node).unwrap();
    let root = ns.root();
    let a = resolve(&ns, &root, b"dir").unwrap();
    let b = resolve(&ns, &root, b"alias").unwrap();
    assert_eq!(a.node().identity(), b.node().identity());
    assert!(!a.same_position(&b));
    ns.attach(&a, fs().root().unwrap()).unwrap();
    assert!(resolve(&ns, &root, b"dir/file").is_err());
    assert!(resolve(&ns, &root, b"alias/file").is_ok());
}
#[test]
fn constructor_validation_and_reference_graph_has_no_strong_cycle() {
    assert!(matches!(
        LocalFs::new(&[LocalEntry {
            parent: 7,
            name: b"x",
            data: None
        }]),
        Err(Error::EINVAL)
    ));
    assert!(matches!(
        LocalFs::new(&[LocalEntry {
            parent: 0,
            name: b"a/b",
            data: None
        }]),
        Err(Error::EINVAL)
    ));
    assert!(matches!(
        LocalFs::new(&[
            LocalEntry {
                parent: 0,
                name: b"x",
                data: Some(b"x")
            },
            LocalEntry {
                parent: 1,
                name: b"y",
                data: None
            }
        ]),
        Err(Error::ENOTDIR)
    ));
    let root = fs().root().unwrap();
    let weak = Arc::downgrade(&root);
    let ns = Namespace::new(root).unwrap();
    let path = resolve(&ns, &ns.root(), b"dir/file").unwrap();
    let opened = OpenFile::open(&path).unwrap();
    drop(path);
    drop(ns);
    assert!(weak.upgrade().is_some());
    drop(opened);
    assert!(weak.upgrade().is_none());
}

#[test]
fn wire_service_references_owner_shape_and_canceled_creation() {
    use crate::service::Service;
    use kcomp_sdk::{
        ipc::service::Request,
        vfs::{codec::*, *},
    };
    let mut service = Service::new(Namespace::new(fs().root().unwrap()).unwrap());
    let mut root_bytes = [0; 40];
    let root_request = Request {
        method: KCOMP_VFS_METHOD_ROOT,
        args: &[],
        input: &[],
        output: 40,
    };
    let undo = service
        .dispatch(7, 70, &root_request, &mut root_bytes)
        .unwrap()
        .unwrap();
    let token = path(&root_bytes[8..]);
    let mut args = [0; 32];
    put_path(&mut args, &token);
    let release = Request {
        method: KCOMP_VFS_METHOD_RELEASE_PATH,
        args: &args,
        input: &[],
        output: 8,
    };
    assert!(matches!(
        service.dispatch(8, 70, &release, &mut [0; 8]),
        Err(Error::EACCES)
    ));
    assert!(matches!(
        service.dispatch(7, 71, &release, &mut [0; 8]),
        Err(Error::EACCES)
    ));
    service.rollback(7, 70, undo);
    assert!(matches!(
        service.dispatch(7, 70, &release, &mut [0; 8]),
        Err(Error::ESTALE)
    ));
    // A malformed root must not consume a reference-table slot.
    let bad = Request {
        method: KCOMP_VFS_METHOD_ROOT,
        args: &[1],
        input: &[],
        output: 40,
    };
    assert!(matches!(
        service.dispatch(7, 70, &bad, &mut root_bytes),
        Err(Error::EINVAL)
    ));
    for _ in 0..40 {
        let undo = service
            .dispatch(7, 70, &root_request, &mut root_bytes)
            .unwrap()
            .unwrap();
        service.rollback(7, 70, undo);
    }
    service
        .dispatch(7, 70, &root_request, &mut root_bytes)
        .unwrap();
    let root = path(&root_bytes[8..]);
    let mut lookup_bytes = [0; 80];
    put_lookup(
        &mut lookup_bytes,
        &VfsLookup {
            start: root,
            root,
            flags: KCOMP_VFS_LOOKUP_CROSS_MOUNTS,
            max_symlinks: 0,
            encoding: KCOMP_VFS_ENCODING_BYTES,
            reserved: 0,
        },
    );
    let resolve = Request {
        method: KCOMP_VFS_METHOD_RESOLVE,
        args: &lookup_bytes,
        input: b"dir/file",
        output: 40,
    };
    service.dispatch(7, 70, &resolve, &mut root_bytes).unwrap();
    let found = path(&root_bytes[8..]);
    let mut open_args = [0; 48];
    put_open(
        &mut open_args,
        &VfsOpenRequest {
            path: found,
            access: KCOMP_VFS_ACCESS_READ,
            share: KCOMP_VFS_SHARE_READ,
            stream_kind: 0,
            encoding: 0,
        },
    );
    let open_request = Request {
        method: KCOMP_VFS_METHOD_OPEN,
        args: &open_args,
        input: &[],
        output: 16,
    };
    let mut opened = [0; 16];
    let undo = service
        .dispatch(7, 70, &open_request, &mut opened)
        .unwrap()
        .unwrap();
    let id = u64_at(&opened, 8);
    let id_bytes = id.to_le_bytes();
    let read = Request {
        method: KCOMP_VFS_METHOD_READ,
        args: &id_bytes,
        input: &[],
        output: 21,
    };
    let mut bytes = [0; 21];
    service.dispatch(7, 70, &read, &mut bytes).unwrap();
    assert_eq!(u64_at(&bytes, 8), 5);
    assert_eq!(&bytes[16..], b"hello");
    service.rollback(7, 70, undo);
    assert!(matches!(
        service.dispatch(7, 70, &read, &mut bytes),
        Err(Error::EBADF)
    ));
    service.reap(|_, _| false);
    assert!(matches!(
        service.dispatch(7, 70, &release, &mut [0; 8]),
        Err(Error::ESTALE)
    ));
}
