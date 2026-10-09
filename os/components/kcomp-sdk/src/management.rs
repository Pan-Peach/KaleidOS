//! ComponentManager / Core truth 的值前端，不交付 registry 或 provider 指针。

use crate::{Errno, Result, abi};
pub use abi::{
    ComponentInfo, DeviceClaimState, DeviceInfo, DeviceSpaceKind, EndpointInfo, ExecutionDomain,
};
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
pub fn create(name: &[u8], domain: ExecutionDomain, config_abi: u64, config: &[u8]) -> Result<u32> {
    let args = abi::KcompCreateArgs {
        config_abi,
        config: config.as_ptr().cast(),
        config_len: config.len(),
    };
    let mut id = 0;
    // SAFETY: name/config/args remain valid for this synchronous call.
    let code = unsafe {
        abi::kcore_component_create(name.as_ptr(), name.len(), domain as u32, &args, &mut id)
    };
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

/// Read a value snapshot without claiming the device or reading its registers.
/// The buffer receives the complete primary compatible, with no NUL terminator.
pub fn device_info(id: u32, compatible: &mut [u8]) -> Result<DeviceInfo> {
    let mut out = MaybeUninit::uninit();
    // SAFETY: both outputs are disjoint, writable, and borrowed for this call.
    let code = unsafe {
        abi::kcore_device_info(
            id,
            out.as_mut_ptr(),
            compatible.as_mut_ptr(),
            compatible.len(),
        )
    };
    if code != 0 {
        return Err(Errno::from_code(code));
    }
    let info = unsafe { out.assume_init() };
    validate_device_info(id, &info, compatible.len())?;
    Ok(info)
}

fn validate_device_info(id: u32, info: &DeviceInfo, capacity: usize) -> Result<()> {
    if info.id != id
        || info.compatible_len as usize > capacity
        || info.state > DeviceClaimState::Quarantined as u32
        || info.space_kind > DeviceSpaceKind::Pio as u32
        || info.irq_known > 1
        || info.reserved != 0
        || (info.state != DeviceClaimState::Claimed as u32 && info.owner != 0)
        || (info.irq_known == 0 && info.irq_line != 0)
        || (info.interrupt_count == 0 && info.irq_known != 0)
        || (info.space_count == 0 && (info.space_kind != 0 || info.base != 0 || info.size != 0))
        || (info.space_count != 0 && info.space_kind == 0)
        || (info.compatible_count == 0 && info.compatible_len != 0)
    {
        return Err(Errno::EIO);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> DeviceInfo {
        DeviceInfo {
            id: 0,
            state: DeviceClaimState::Unclaimed as u32,
            owner: 0,
            compatible_len: 8,
            compatible_count: 1,
            space_count: 1,
            interrupt_count: 1,
            space_kind: DeviceSpaceKind::Mmio as u32,
            irq_known: 1,
            base: 0x1000,
            size: 0x1000,
            irq_line: 0,
            reserved: 0,
        }
    }

    #[test]
    fn device_reply_validation_preserves_zero_id_and_zero_irq() {
        assert_eq!(validate_device_info(0, &info(), 8), Ok(()));
        let mut claimed = info();
        claimed.state = DeviceClaimState::Claimed as u32;
        claimed.owner = 0;
        assert_eq!(validate_device_info(0, &claimed, 8), Ok(()));
        claimed.state = DeviceClaimState::Quarantined as u32;
        assert_eq!(validate_device_info(0, &claimed, 8), Ok(()));
    }

    #[test]
    fn device_reply_validation_rejects_malformed_snapshot_before_slicing() {
        let valid = info();
        assert_eq!(validate_device_info(1, &valid, 8), Err(Errno::EIO));
        assert_eq!(validate_device_info(0, &valid, 7), Err(Errno::EIO));
        for malformed in [
            DeviceInfo { state: 3, ..valid },
            DeviceInfo {
                space_kind: 3,
                ..valid
            },
            DeviceInfo {
                reserved: 1,
                ..valid
            },
            DeviceInfo { owner: 99, ..valid },
            DeviceInfo {
                irq_known: 2,
                ..valid
            },
            DeviceInfo {
                irq_known: 0,
                irq_line: 10,
                ..valid
            },
            DeviceInfo {
                interrupt_count: 0,
                ..valid
            },
            DeviceInfo {
                space_count: 0,
                ..valid
            },
            DeviceInfo {
                space_kind: 0,
                ..valid
            },
            DeviceInfo {
                compatible_count: 0,
                ..valid
            },
        ] {
            assert_eq!(validate_device_info(0, &malformed, 8), Err(Errno::EIO));
        }
    }
}
