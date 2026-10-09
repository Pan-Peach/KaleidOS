use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use kcomp_sdk::endpoint::{Endpoint, InvokeError};
use kcomp_sdk::filesystem::{FILESYSTEM_NAME, FileSystem};
use kcomp_sdk::management::{self, ExecutionDomain};
use kcomp_sdk::scheduler::{self, SCHEDULER_POLICY_NAME, SchedulerPolicy};
use kcomp_sdk::{Errno, Result};

// FatFs create config is one native-endian u64 opaque block EndpointId.
const FATFS_CONFIG_ABI: u64 = 0x4641_5446_5343_4647;
static FILESYSTEM_PROVIDER: AtomicU32 = AtomicU32::new(0);
static MOUNT_STATUS: AtomicI32 = AtomicI32::new(Errno::EAGAIN.code());

extern "C" fn mount_root(_arg: *mut ()) {
    let provider = FILESYSTEM_PROVIDER.load(Ordering::Acquire);
    let result = Endpoint::<FileSystem>::lookup(provider, FILESYSTEM_NAME)
        .map_err(InvokeError::Transport)
        .and_then(|endpoint| endpoint.bind())
        .and_then(|filesystem| filesystem.mount());
    let status = match result {
        Ok(()) => 0,
        Err(error) => {
            kcomp_sdk::klog!("init: root mount failed: {error:?}");
            match error {
                InvokeError::Transport(errno) | InvokeError::Method(errno) => errno.code(),
                InvokeError::InvalidReply => Errno::EIO.code(),
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
    // The current prober has one finite, non-yielding dispatch task. run_tasks
    // is not a join; an asynchronous prober will need a completion contract.
    management::run_tasks()?;
    let mut root_filesystem = 0;
    if let Some(block) = root_endpoint(root_ordinal)? {
        let filesystem = management::create(
            b"fatfs",
            ExecutionDomain::KernelNative,
            FATFS_CONFIG_ABI,
            &block.to_ne_bytes(),
        )?;
        FILESYSTEM_PROVIDER.store(filesystem, Ordering::Release);
        root_filesystem = filesystem;
        management::start_task(mount_root)?;
        management::run_tasks()?;
        let status = MOUNT_STATUS.load(Ordering::Acquire);
        if status != 0 {
            return Err(Errno::from_code(status));
        }
        kcomp_sdk::klog!("init: FAT root mounted");
    } else {
        kcomp_sdk::klog!("init: no block device; console session only");
    }
    let endpoint = if root_filesystem == 0 {
        0
    } else {
        Endpoint::<FileSystem>::lookup(root_filesystem, FILESYSTEM_NAME)?.id()
    };
    let shell = management::create(
        b"ksh",
        ExecutionDomain::KernelNative,
        0x4B53_4846_5343_4647,
        &endpoint.to_ne_bytes(),
    )?;
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
