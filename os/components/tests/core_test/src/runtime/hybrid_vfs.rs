//! Real C FatFs images, two block instances and LocalFs in one VFS Server Task.
use super::report::Checks;
use core::sync::atomic::{AtomicU32, Ordering};
use kcomp_sdk::{
    Errno, abi,
    block::{BLOCK_DEVICE_NAME, BlockDevice},
    endpoint::Endpoint,
    filesystem::{FILESYSTEM_NAME, FileSystem},
    generated::filesystem::*,
    ipc, management, mem,
    vfs::{codec, *},
};
const FAT_BYTES: &[u8] = b"KALEIDOS BLOCK CHAIN OK";
const LOCAL_BYTES: &[u8] = b"KaleidOS local filesystem\n";
struct State {
    vfs: u64,
    fat: [u64; 2],
    fresh: u64,
    result: AtomicU32,
}
struct Foreign {
    vfs: u64,
    root: VfsPath,
    file: u64,
    result: AtomicU32,
}
struct Graph {
    vfs: u32,
    fats: [u32; 2],
    blocks: [u32; 2],
    endpoint: u64,
    fat: [u64; 2],
}
fn start(entry: abi::KcompTaskEntry, arg: *mut ()) -> Option<u32> {
    let mut task = 0;
    (unsafe { abi::kcore_task_create(entry, arg, &mut task) } == 0
        && unsafe { abi::kcore_task_start(task) } == 0)
        .then_some(task)
}
fn finish(task: u32, in_task: bool) -> bool {
    let deadline = unsafe { abi::kcore_now() + abi::kcore_timebase_hz() * 10 };
    while unsafe { abi::kcore_task_state(task) } != 4 {
        if unsafe { abi::kcore_now() } >= deadline {
            return false;
        }
        let result = if in_task {
            management::yield_task()
        } else {
            management::run_tasks()
        };
        if result.is_err() {
            return false;
        }
    }
    true
}
fn binding(endpoint: u64) -> VfsBinding {
    VfsBinding::bind(Endpoint::<Vfs>::from_id(endpoint).unwrap()).unwrap()
}
fn resolve(vfs: &VfsBinding, root: VfsPath, path: &[u8]) -> VfsResult<VfsPath> {
    vfs.resolve(
        &VfsLookup {
            start: root,
            root,
            flags: KCOMP_VFS_LOOKUP_CROSS_MOUNTS,
            max_symlinks: 0,
            encoding: KCOMP_VFS_ENCODING_BYTES,
            reserved: 0,
        },
        path,
    )
}
fn open(vfs: &VfsBinding, path: VfsPath) -> VfsResult<u64> {
    vfs.open(
        &VfsOpenRequest {
            path,
            access: KCOMP_VFS_ACCESS_READ,
            share: KCOMP_VFS_SHARE_READ,
            stream_kind: KCOMP_VFS_STREAM_DEFAULT,
            encoding: 0,
        },
        &[],
    )
}
fn exact(vfs: &VfsBinding, path: &[u8], expected: &[u8]) -> bool {
    let Ok(file) = vfs.open_path(path) else {
        return false;
    };
    let mut bytes = [0; 64];
    let ok = vfs.read(file, &mut bytes) == Ok(expected.len())
        && bytes[..expected.len()] == *expected
        && vfs.read(file, &mut bytes) == Ok(0);
    ok && vfs.close(file).is_ok()
}
extern "C" fn foreign(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<Foreign>() };
    let vfs = binding(state.vfs);
    state.result.store(
        u32::from(
            vfs.node_info(&state.root) == Err(VfsError::Method(Errno::EACCES.code()))
                && vfs.read(state.file, &mut [0; 1]) == Err(VfsError::Method(Errno::EACCES.code()))
                && vfs.close(state.file) == Err(VfsError::Method(Errno::EACCES.code())),
        ),
        Ordering::Release,
    );
    management::exit_task();
}
fn foreign_task(vfs: &VfsBinding, root: VfsPath, file: u64) -> bool {
    let Ok(region) = mem::mem_alloc(
        core::mem::size_of::<Foreign>() as u64,
        core::mem::align_of::<Foreign>() as u64,
    ) else {
        return false;
    };
    let state = region.base as *mut Foreign;
    unsafe {
        state.write(Foreign {
            vfs: vfs.endpoint.id(),
            root,
            file,
            result: AtomicU32::new(0),
        })
    };
    let done = start(foreign, state.cast()).is_some_and(|task| finish(task, true));
    let ok = done && unsafe { (*state).result.load(Ordering::Acquire) == 1 };
    // A failed start may leave a Created Task borrowing its argument.
    if done {
        let _ = mem::mem_release(region);
    }
    ok
}
fn canceled_opens(vfs: &VfsBinding, path: VfsPath) -> bool {
    let mut request = [0; 64];
    codec::put32(&mut request, 0, KCOMP_VFS_METHOD_OPEN);
    codec::put32(&mut request, 4, 16);
    codec::put32(&mut request, 8, 48);
    codec::put_open(
        &VfsOpenRequest {
            path,
            access: KCOMP_VFS_ACCESS_READ,
            share: KCOMP_VFS_SHARE_READ,
            stream_kind: KCOMP_VFS_STREAM_DEFAULT,
            encoding: 0,
        },
        &mut request[16..],
    )
    .unwrap();
    // Exceed both the VFS table and FatFs open budget. A lost creation reply
    // must roll back its Path/Open reference before the next served request.
    for _ in 0..40 {
        let Ok(receipt) = ipc::submit(vfs.endpoint.id(), &request) else {
            return false;
        };
        let canceled = ipc::cancel(receipt);
        let mut reply = [0; 20];
        let result = loop {
            match ipc::collect(receipt, &mut reply) {
                Err(Errno::EAGAIN) => {
                    if ipc::wait_request(receipt).is_err() {
                        return false;
                    }
                }
                result => break result,
            }
        };
        match (canceled, result) {
            (Ok(()), Err(Errno::ECANCELED)) => {}
            (Err(Errno::EALREADY), Ok(20)) if codec::u32_at(&reply, 0) == 0 => {
                if vfs.close(codec::u64_at(&reply, 12)).is_err() {
                    return false;
                }
            }
            _ => return false,
        }
        let Ok(probe) = open(vfs, path) else {
            return false;
        };
        if vfs.close(probe).is_err() {
            return false;
        }
    }
    true
}
extern "C" fn client(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let vfs = binding(state.vfs);
    let mut bits = 0;
    let root = vfs.root().unwrap();
    let first = resolve(&vfs, root, b"/fat/HELLO.TXT").unwrap();
    let alias = resolve(&vfs, root, b"/fat/hello.txt").unwrap();
    let second = resolve(&vfs, root, b"/second/HELLO.TXT").unwrap();
    if first == alias && first.fs != second.fs && first.mount != second.mount {
        bits |= 1;
    }
    if exact(&vfs, b"/local/README.TXT", LOCAL_BYTES)
        && exact(&vfs, b"/fat/HELLO.TXT", FAT_BYTES)
        && exact(&vfs, b"/second/HELLO.TXT", FAT_BYTES)
    {
        bits |= 2;
    }
    let a = open(&vfs, first).unwrap();
    let b = open(&vfs, first).unwrap();
    let mut bytes = [0; 4];
    if vfs.read(a, &mut bytes[..1]) == Ok(1)
        && bytes[0] == b'K'
        && vfs.read_at(a, 3, &mut bytes[..1]) == Ok(1)
        && bytes[0] == b'E'
        && vfs.read(a, &mut bytes[..1]) == Ok(1)
        && bytes[0] == b'A'
        && vfs.read(b, &mut bytes) == Ok(4)
        && bytes == *b"KALE"
        && vfs.read_at(a, FAT_BYTES.len() as u64, &mut bytes) == Ok(0)
        && vfs.read(a, &mut []) == Ok(0)
    {
        bits |= 4;
    }
    if foreign_task(&vfs, root, a) {
        bits |= 8;
    }
    if vfs.close(a).is_ok()
        && vfs.close(b).is_ok()
        && vfs.close(a) == Err(VfsError::Method(Errno::EBADF.code()))
        && vfs.read(a, &mut bytes) == Err(VfsError::Method(Errno::EBADF.code()))
    {
        bits |= 16;
    }
    if resolve(&vfs, root, b"/fat/ABSENT.TXT") == Err(VfsError::Method(Errno::ENOENT.code()))
        && resolve(&vfs, root, b"/fat/HELLO.TXT/child")
            == Err(VfsError::Method(Errno::ENOTDIR.code()))
        && resolve(&vfs, root, b"/fat/HELLO.TXT/") == Err(VfsError::Method(Errno::ENOTDIR.code()))
    {
        bits |= 32;
    }
    if canceled_opens(&vfs, first) {
        bits |= 64;
    }
    let held = open(&vfs, first).unwrap();
    if ipc::service::invoke(
        state.fat[0],
        KCOMP_FILESYSTEM_METHOD_SHUTDOWN,
        &[],
        &[],
        &mut [],
    ) == Ok(0)
        && vfs.read(held, &mut bytes).is_err()
        && vfs.close(held).is_ok()
        && vfs.close(held) == Err(VfsError::Method(Errno::EBADF.code()))
        && exact(&vfs, b"/second/HELLO.TXT", FAT_BYTES)
        && exact(&vfs, b"/local/README.TXT", LOCAL_BYTES)
    {
        bits |= 128;
    }
    let _ = vfs.release_path(&first);
    let _ = vfs.release_path(&alias);
    let _ = vfs.release_path(&second);
    let _ = vfs.release_path(&root);
    state.result.store(bits, Ordering::Release);
    management::exit_task();
}
extern "C" fn abandon(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let vfs = binding(state.vfs);
    // Leave both a provider open and two VFS path references to the next
    // request's verified Task liveness sweep.
    let ok = vfs.root().is_ok() && vfs.open_path(b"/second/HELLO.TXT").is_ok();
    state.result.store(u32::from(ok), Ordering::Release);
    management::exit_task();
}
extern "C" fn shutdown(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let ok = binding(state.vfs).shutdown().is_ok()
        && ipc::service::invoke(
            state.fat[1],
            KCOMP_FILESYSTEM_METHOD_SHUTDOWN,
            &[],
            &[],
            &mut [],
        ) == Ok(0)
        && ipc::service::invoke(
            state.fresh,
            KCOMP_FILESYSTEM_METHOD_SHUTDOWN,
            &[],
            &[],
            &mut [],
        ) == Ok(0);
    state.result.store(u32::from(ok), Ordering::Release);
    management::exit_task();
}
extern "C" fn restarted(arg: *mut ()) {
    let state = unsafe { &*arg.cast::<State>() };
    let mut root = [0; 8];
    let mut node = [0; 8];
    let mut args = [0; 12];
    let mut handle = [0; 8];
    let mounted = ipc::service::invoke(
        state.fresh,
        KCOMP_FILESYSTEM_METHOD_MOUNT,
        &[],
        &[],
        &mut [],
    ) == Ok(0)
        && ipc::service::invoke(
            state.fresh,
            KCOMP_FILESYSTEM_METHOD_ROOT,
            &[],
            &[],
            &mut root,
        ) == Ok(0);
    args[..8].copy_from_slice(&root);
    args[8..].copy_from_slice(&KCOMP_FILESYSTEM_ENCODING_BYTES.to_le_bytes());
    let opened = mounted
        && ipc::service::invoke(
            state.fresh,
            KCOMP_FILESYSTEM_METHOD_LOOKUP,
            &args,
            b"HELLO.TXT",
            &mut node,
        ) == Ok(0)
        && ipc::service::invoke(
            state.fresh,
            KCOMP_FILESYSTEM_METHOD_OPEN_NODE,
            &node,
            &[],
            &mut handle,
        ) == Ok(0);
    let mut read_args = [0; 16];
    read_args[..8].copy_from_slice(&handle);
    let mut reply = [0; 40];
    let ok = opened
        && state.fresh != state.fat[0]
        && ipc::service::invoke(
            state.fresh,
            KCOMP_FILESYSTEM_METHOD_READ_AT,
            &read_args,
            &[],
            &mut reply,
        ) == Ok(0)
        && codec::u64_at(&reply, 0) == FAT_BYTES.len() as u64
        && reply[8..8 + FAT_BYTES.len()] == *FAT_BYTES
        && ipc::service::invoke(
            state.fat[0],
            KCOMP_FILESYSTEM_METHOD_READ_AT,
            &read_args,
            &[],
            &mut reply,
        ) == Err(Errno::ENOENT)
        && binding(state.vfs).open_path(b"/fat/HELLO.TXT").is_err();
    if opened {
        let _ = ipc::service::invoke(
            state.fresh,
            KCOMP_FILESYSTEM_METHOD_CLOSE,
            &handle,
            &[],
            &mut [],
        );
    }
    state.result.store(u32::from(ok), Ordering::Release);
    management::exit_task();
}
fn create_fat(owner: u32, block: u64) -> kcomp_sdk::Result<u32> {
    let mut config = [0; 16];
    config[..8].copy_from_slice(&block.to_le_bytes());
    config[8..12].copy_from_slice(&owner.to_le_bytes());
    config[12..].copy_from_slice(&1u32.to_le_bytes());
    management::create(
        b"fatfs",
        management::ExecutionDomain::KernelNative,
        KCOMP_FATFS_CREATE_CONFIG_ABI,
        &config,
    )
}
fn compose(owner: u32) -> kcomp_sdk::Result<Graph> {
    let mut blocks = [0; 2];
    let mut fats = [0; 2];
    let mut endpoints = [0; 2];
    for index in 0..2 {
        blocks[index] = management::load(b"ram_blk", management::ExecutionDomain::KernelNative)?;
        let block = Endpoint::<BlockDevice>::lookup(blocks[index], BLOCK_DEVICE_NAME)?;
        fats[index] = create_fat(owner, block.id())?;
        endpoints[index] = Endpoint::<FileSystem>::lookup(fats[index], FILESYSTEM_NAME)?.id();
    }
    management::run_tasks()?;
    let mut config = [0; 24];
    config[..4].copy_from_slice(&owner.to_le_bytes());
    config[4..8].copy_from_slice(&2u32.to_le_bytes());
    config[8..16].copy_from_slice(&endpoints[0].to_le_bytes());
    config[16..].copy_from_slice(&endpoints[1].to_le_bytes());
    let vfs = management::create(
        b"vfs",
        management::ExecutionDomain::KernelNative,
        KCOMP_VFS_CREATE_CONFIG_ABI,
        &config,
    )?;
    for endpoint in endpoints {
        ipc::grant(endpoint, vfs)?;
    }
    management::run_tasks()?;
    Ok(Graph {
        vfs,
        fats,
        blocks,
        endpoint: Endpoint::<Vfs>::lookup(vfs, VFS_NAME)?.id(),
        fat: endpoints,
    })
}
pub fn group(checks: &mut Checks) {
    checks.group("hybrid-vfs");
    let owner = management::current_component().unwrap();
    let Ok(Graph {
        vfs,
        fats,
        blocks,
        endpoint,
        fat,
    }) = compose(owner)
    else {
        checks.check("hybrid-compose", false);
        return;
    };
    let region = mem::mem_alloc(
        core::mem::size_of::<State>() as u64,
        core::mem::align_of::<State>() as u64,
    )
    .unwrap();
    let state = region.base as *mut State;
    unsafe {
        state.write(State {
            vfs: endpoint,
            fat,
            fresh: 0,
            result: AtomicU32::new(0),
        })
    };
    let done = start(client, state.cast()).is_some_and(|task| finish(task, false));
    let bits = unsafe { (*state).result.load(Ordering::Acquire) };
    for (name, mask) in [
        ("hybrid-node-instance-identity", 1),
        ("hybrid-local-remote-reads", 2),
        ("hybrid-independent-cursors-eof", 4),
        ("hybrid-wrong-task-rejected", 8),
        ("hybrid-close-stale", 16),
        ("hybrid-lookup-errors", 32),
        ("hybrid-canceled-open-rollback", 64),
        ("hybrid-provider-loss", 128),
    ] {
        checks.check(name, done && bits & mask != 0);
    }
    let new_block = Endpoint::<BlockDevice>::lookup(blocks[0], BLOCK_DEVICE_NAME).unwrap();
    let fresh_id = create_fat(owner, new_block.id()).unwrap();
    let _ = management::run_tasks();
    let fresh_endpoint = Endpoint::<FileSystem>::lookup(fresh_id, FILESYSTEM_NAME)
        .unwrap()
        .id();
    // client has exited; no Task still borrows State during this anchor update.
    if done {
        unsafe { (*state).fresh = fresh_endpoint };
    }
    let restart_done =
        done && start(restarted, state.cast()).is_some_and(|task| finish(task, false));
    checks.check(
        "hybrid-provider-restart-no-rebind",
        restart_done && unsafe { (*state).result.load(Ordering::Acquire) == 1 },
    );
    let mut reaped = done;
    // A leak would exceed both the 8-open provider and 32-reference VFS limits.
    for _ in 0..40 {
        let exited = start(abandon, state.cast()).is_some_and(|task| finish(task, false));
        reaped &= exited && unsafe { (*state).result.load(Ordering::Acquire) == 1 };
        if !exited {
            break;
        }
    }
    checks.check("hybrid-caller-exit-reaps", reaped);
    let stopped = start(shutdown, state.cast()).is_some_and(|task| finish(task, false));
    checks.check(
        "hybrid-server-drain",
        stopped && unsafe { (*state).result.load(Ordering::Acquire) == 1 },
    );
    // All server Tasks run on this CPU. Run once more to retire shutdown's reply
    // before using the public stop admission, without private Task inspection.
    let _ = management::run_tasks();
    let mut closed = true;
    for id in [vfs, fats[0], fats[1], fresh_id] {
        closed &= unsafe { abi::kcore_component_stop(id) } == 0;
    }
    checks.check("hybrid-ipc-only-stop", closed);
    // Existing block fixtures publish Direct tables and intentionally stay resident.
    let _ = blocks;
    if done && reaped && stopped {
        let _ = mem::mem_release(region);
    }
}
