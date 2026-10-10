use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use kcomp_sdk::endpoint::Endpoint;
use kcomp_sdk::filesystem::{FILESYSTEM_NAME, FileSystem};
use kcomp_sdk::generated::filesystem::KCOMP_FATFS_CREATE_CONFIG_ABI;
use kcomp_sdk::management::{self, ExecutionDomain};
use kcomp_sdk::scheduler::{self, SCHEDULER_POLICY_NAME, SchedulerPolicy};
use kcomp_sdk::vfs::{KCOMP_VFS_CREATE_CONFIG_ABI, VFS_NAME, Vfs, VfsError};
use kcomp_sdk::{Errno, Result};

static FILESYSTEM_PROVIDER: AtomicU32 = AtomicU32::new(0);
static MOUNT_STATUS: AtomicI32 = AtomicI32::new(Errno::EAGAIN.code());

extern "C" fn mount_root(_arg: *mut ()) {
    let provider = FILESYSTEM_PROVIDER.load(Ordering::Acquire);
    let result = Endpoint::<Vfs>::lookup(provider, VFS_NAME)
        .map_err(VfsError::Transport)
        .and_then(|ep| kcomp_sdk::vfs::VfsBinding::bind(ep).map_err(VfsError::Transport))
        .and_then(|vfs| {
            let root = vfs.root()?;
            vfs.release_path(&root)
        });
    let status = match result {
        Ok(()) => 0,
        Err(error) => {
            kcomp_sdk::klog!("init: root mount failed: {error:?}");
            match error {
                VfsError::Transport(errno) => errno.code(),
                VfsError::Method(code) | VfsError::Domain { errno: code, .. } => code,
                _ => Errno::EIO.code(),
            }
        }
    };
    MOUNT_STATUS.store(status, Ordering::Release);
    management::exit_task();
}

fn root_endpoint(mut ordinal_wanted: u32) -> Result<Option<u64>> {
    let requested = ordinal_wanted;
    let mut selected = None;
    let mut name = [0; 256];
    for ordinal in 0..u32::MAX {
        let Some(row) = management::endpoint_nth(ordinal, &mut name)? else {
            break;
        };
        selected = crate::root::observe(&mut ordinal_wanted, selected, &row);
    }
    if selected.is_none() && requested != 0 {
        return Err(Errno::ENODEV);
    }
    Ok(selected)
}

fn compose(root_ordinal: u32) -> Result<()> {
    // Reject task-context re-entry before changing the graph. Boot is an anchor.
    management::run_tasks()?;
    let policy = management::load(b"scheduler_rr", ExecutionDomain::KernelNative)?;
    scheduler::select(&Endpoint::<SchedulerPolicy>::lookup(
        policy,
        SCHEDULER_POLICY_NAME,
    )?)?;
    kcomp_sdk::klog!("init: scheduler ready");

    management::load(b"driver_prober", ExecutionDomain::KernelNative)?;
    // The prober has one finite dispatch Task and pulls IPC results. run_tasks
    // drains runnable work; it does not join an arbitrary future background prober.
    management::run_tasks()?;
    let owner = management::current_component()?;
    let filesystem = if let Some(block) = root_endpoint(root_ordinal)? {
        let mut config = [0; 16];
        config[..8].copy_from_slice(&block.to_le_bytes());
        config[8..12].copy_from_slice(&owner.to_le_bytes());
        let fat = management::create(
            b"fatfs",
            ExecutionDomain::KernelNative,
            KCOMP_FATFS_CREATE_CONFIG_ABI,
            &config,
        )?;
        kcomp_sdk::ipc::grant(block, fat)?;
        Some(fat)
    } else {
        None
    };
    // Let Fat's owned server register its listener before composition grants.
    management::run_tasks()?;
    let mut config = alloc::vec::Vec::new();
    config.extend_from_slice(&owner.to_le_bytes());
    config.extend_from_slice(&u32::from(filesystem.is_some()).to_le_bytes());
    let fs_endpoint = filesystem
        .map(|id| Endpoint::<FileSystem>::lookup(id, FILESYSTEM_NAME))
        .transpose()?
        .map(|ep| ep.id());
    if let Some(endpoint) = fs_endpoint {
        config.extend_from_slice(&endpoint.to_le_bytes());
    }
    let vfs = management::create(
        b"vfs",
        ExecutionDomain::KernelNative,
        KCOMP_VFS_CREATE_CONFIG_ABI,
        &config,
    )?;
    if let Some(endpoint) = fs_endpoint {
        kcomp_sdk::ipc::grant(endpoint, vfs)?;
    }
    FILESYSTEM_PROVIDER.store(vfs, Ordering::Release);
    management::run_tasks()?;
    management::start_task(mount_root)?;
    management::run_tasks()?;
    let status = MOUNT_STATUS.load(Ordering::Acquire);
    if status != 0 {
        return Err(Errno::from_code(status));
    }
    if filesystem.is_some() {
        kcomp_sdk::klog!("init: FAT root mounted");
    } else {
        kcomp_sdk::klog!("init: no block device; console session only");
    }
    let endpoint = Endpoint::<Vfs>::lookup(vfs, VFS_NAME)?.id();
    let mut shell_config = alloc::vec::Vec::from(endpoint.to_le_bytes());
    shell_config.extend_from_slice(if filesystem.is_some() { b"/fat" } else { b"/" });
    let shell = management::create(
        b"ksh",
        ExecutionDomain::KernelNative,
        0x72BD_51C9_340F_A806,
        &shell_config,
    )?;
    kcomp_sdk::ipc::grant(endpoint, shell)?;
    kcomp_sdk::klog!("init: ksh queued (id={shell})");
    Ok(())
}

kcomp_sdk::kcomp_instance_create!(|args, _out_state| {
    // SAFETY: Core provides this header for the duration of create.
    let Some(args) = (unsafe { args.as_ref() }) else {
        return Errno::EINVAL.code();
    };
    let root_ordinal = if args.config_abi == 0 && args.config_len == 0 {
        0 // Default boot profile: first discovered block endpoint.
    } else if args.config_abi == 0x494E_4954_524F_4F54
        && args.config_len == 4
        && !args.config.is_null()
    {
        // SAFETY: Core borrows four valid config bytes for this call; no alignment required.
        unsafe { core::ptr::read_unaligned(args.config.cast::<u32>()) }
    } else {
        return Errno::EINVAL.code();
    };
    match compose(root_ordinal) {
        Ok(()) => 0,
        Err(error) => {
            kcomp_sdk::klog!("init: startup failed: {error}");
            error.code()
        }
    }
});
kcomp_sdk::kcomp_instance_destroy!(|_state| { 0 });
