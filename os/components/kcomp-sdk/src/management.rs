//! ComponentManager / Core truth 的值前端，不交付 registry 或 provider 指针。

use crate::{Errno, Result, abi};
pub use abi::{ComponentInfo, EndpointInfo, ExecutionDomain};
use core::mem::MaybeUninit;

pub fn load(name: &[u8], domain: ExecutionDomain) -> Result<u32> {
    // SAFETY: name is borrowed only for this call; domain is a wire value.
    let code = unsafe { abi::kcore_component_load(name.as_ptr(), name.len(), domain as u32) };
    if code < 0 {
        Err(Errno::from_code(code))
    } else {
        Ok(code as u32)
    }
}

/// Flat create config borrowed only until the provider's create returns.
pub fn create(name: &[u8], config_abi: u64, config: &[u8]) -> Result<u32> {
    let args = abi::KcompCreateArgs {
        config_abi,
        config: config.as_ptr().cast(),
        config_len: config.len(),
    };
    let mut id = 0;
    // SAFETY: name/config/args remain valid for this synchronous call.
    let code = unsafe { abi::kcore_component_create(name.as_ptr(), name.len(), &args, &mut id) };
    if code == 0 {
        Ok(id)
    } else {
        Err(Errno::from_code(code))
    }
}

pub fn component_nth(ordinal: u32, name: &mut [u8]) -> Result<Option<ComponentInfo>> {
    let mut out = MaybeUninit::uninit();
    // SAFETY: outputs are caller-owned for this call.
    let code = unsafe {
        abi::kcore_component_nth(ordinal, out.as_mut_ptr(), name.as_mut_ptr(), name.len())
    };
    if code == Errno::ENOENT.code() {
        return Ok(None);
    }
    if code != 0 {
        return Err(Errno::from_code(code));
    }
    // SAFETY: success initializes the whole projection.
    let info: ComponentInfo = unsafe { out.assume_init() };
    if info.name_len as usize > name.len() {
        return Err(Errno::EIO);
    }
    Ok(Some(info))
}

pub fn endpoint_nth(ordinal: u32, name: &mut [u8]) -> Result<Option<EndpointInfo>> {
    let mut out = MaybeUninit::uninit();
    // SAFETY: outputs are caller-owned for this call.
    let code = unsafe {
        abi::kcore_endpoint_nth(ordinal, out.as_mut_ptr(), name.as_mut_ptr(), name.len())
    };
    if code == Errno::ENOENT.code() {
        return Ok(None);
    }
    if code != 0 {
        return Err(Errno::from_code(code));
    }
    let info: EndpointInfo = unsafe { out.assume_init() };
    if info.name_len as usize > name.len() {
        return Err(Errno::EIO);
    }
    Ok(Some(info))
}

pub fn device_nth(ordinal: u32) -> Result<Option<u32>> {
    let mut id = 0;
    // SAFETY: empty non-null slice requests unfiltered discovery; out is writable.
    let code = unsafe { abi::kcore_device_nth(b"".as_ptr(), 0, ordinal, &mut id) };
    match code {
        0 => Ok(Some(id)),
        n if n == Errno::ENOENT.code() => Ok(None),
        n => Err(Errno::from_code(n)),
    }
}

/// 无参数任务入口；Core 验证入口位于 caller 镜像并提交 owner / Runnable。
pub fn start_task(entry: abi::KcompTaskEntry) -> Result<u32> {
    let mut id = 0;
    let code = unsafe { abi::kcore_task_create(entry, core::ptr::null_mut(), &mut id) };
    if code != 0 {
        return Err(Errno::from_code(code));
    }
    let code = unsafe { abi::kcore_task_start(id) };
    if code != 0 {
        return Err(Errno::from_code(code));
    }
    Ok(id)
}

pub fn yield_task() -> Result<()> {
    let code = unsafe { abi::kcore_task_yield() };
    if code == 0 {
        Ok(())
    } else {
        Err(Errno::from_code(code))
    }
}

/// Enter scheduling from an anchor (not from a task or service callback).
/// Returns when control reaches that anchor again; this is not a task join.
pub fn run_tasks() -> Result<()> {
    let code = unsafe { abi::kcore_sched_run() };
    if code == 0 {
        Ok(())
    } else {
        Err(Errno::from_code(code))
    }
}

pub fn exit_task() -> ! {
    let _ = unsafe { abi::kcore_task_exit() };
    loop {
        core::hint::spin_loop();
    }
}
