//! host 测试的 Core ABI 替身（`#[cfg(test)]` 限定）。
//!
//! kcomp-sdk 的 host 测试无法链接真实 Core；这里用**同一测试二进制内的符号定义**
//! 满足 `generated::abi` 的 extern 声明：
//!
//! - `kcore_endpoint_validate` / `kcore_endpoint_lookup`：行为只由参数决定
//!   （无共享状态），任何测试可并发调用；
//! - `kcore_endpoint_call` / `kcore_endpoint_bind`：行为由脚本变量决定、并记录最近
//!   一次调用——使用它们的测试必须持有 [`lock`]（串行化），避免互相踩；
//! - `kcore_heap_alloc` / `kcore_heap_dealloc`：recording 替身 + `std::alloc`
//!   按真实 layout 分配，验证 KernelNative `GlobalAlloc` adapter 的 ABI 契约。
//!
//! 这些替身**只存在于测试构建**；组件镜像链接的是真实 Core 导出。

use std::sync::Mutex;
use std::vec::Vec;

// Legacy SDK tests link the new Block backend even when they exercise only
// Direct/Gate. Unsupported IPC fails explicitly; these stubs do not simulate
// Exchange, scheduling, or isolation. Real envelope compatibility is tested in
// tests/build/test_ipc_codec.py and transport behavior in CoreTest.
#[unsafe(no_mangle)]
pub extern "C" fn kcore_ipc_submit(_: u64, _: *const u8, _: usize, _: *mut u64) -> i32 {
    crate::Errno::ENOTSUP.code()
}
#[unsafe(no_mangle)]
pub extern "C" fn kcore_ipc_collect(
    _: u64,
    _: *mut u8,
    _: usize,
    _: *mut usize,
    _: *mut i32,
) -> i32 {
    crate::Errno::ENOTSUP.code()
}
#[unsafe(no_mangle)]
pub extern "C" fn kcore_ipc_wait(_: u64, _: u64) -> i32 {
    crate::Errno::ENOTSUP.code()
}
#[unsafe(no_mangle)]
pub extern "C" fn kcore_ipc_cancel(_: u64) -> i32 {
    crate::Errno::ENOTSUP.code()
}

/// `kcore_endpoint_publish` 的脚本回复（status）与最近一次入参快照。
static PUBLISH_SCRIPT: Mutex<i32> = Mutex::new(0);
static LAST_PUBLISH: Mutex<Option<PublishRecord>> = Mutex::new(None);
/// `kcore_memory_acquire` 的脚本回复：`(status, base, len)`。
static MEM_ACQUIRE_SCRIPT: Mutex<(i32, usize, usize)> = Mutex::new((0, 0, 0));
/// `kcore_memory_release` 的脚本回复（status）。
static MEM_RELEASE_SCRIPT: Mutex<i32> = Mutex::new(0);
/// `kcore_heap_alloc` 的脚本回复：`0` = 真实分配；非 0 = 模拟耗尽（返回 null）。
static HEAP_ALLOC_SCRIPT: Mutex<i32> = Mutex::new(0);
/// 最近一次 `kcore_heap_alloc` 的入参快照。
static LAST_HEAP_ALLOC: Mutex<Option<HeapAllocRecord>> = Mutex::new(None);
/// 最近一次 `kcore_heap_dealloc` 的入参快照。
static LAST_HEAP_DEALLOC: Mutex<Option<HeapDeallocRecord>> = Mutex::new(None);
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 最近一次 `kcore_heap_alloc` 的入参快照。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeapAllocRecord {
    pub size: usize,
    pub align: usize,
}

/// 最近一次 `kcore_heap_dealloc` 的入参快照（`ptr` 以整数保存，便于断言）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeapDeallocRecord {
    pub ptr: usize,
    pub size: usize,
    pub align: usize,
}

/// 最近一次 `kcore_endpoint_publish` 的入参快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishRecord {
    pub port_name: Vec<u8>,
    pub contract: u64,
    pub kind: u32,
    pub abi: u64,
    pub port: u32,
    pub api: usize,
    pub ctx: usize,
}

/// 串行化所有使用 `kcore_endpoint_call` 脚本的测试。
pub(crate) fn lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 复位脚本：下一次调用返回 `(transport = 0, method = 0)`、bind 返回
/// `(status = 0, mechanism = 0, api = 0, ctx = 0)`，并清空记录。
pub(crate) fn reset_script() {
    *PUBLISH_SCRIPT.lock().unwrap() = 0;
    *LAST_PUBLISH.lock().unwrap() = None;
    *MEM_ACQUIRE_SCRIPT.lock().unwrap() = (0, 0, 0);
    *MEM_RELEASE_SCRIPT.lock().unwrap() = 0;
    *HEAP_ALLOC_SCRIPT.lock().unwrap() = 0;
    *LAST_HEAP_ALLOC.lock().unwrap() = None;
    *LAST_HEAP_DEALLOC.lock().unwrap() = None;
}

/// 模拟 `kcore_heap_alloc` 耗尽：下一次调用返回 null（不影响 dealloc）。
pub(crate) fn script_heap_exhaustion() {
    *HEAP_ALLOC_SCRIPT.lock().unwrap() = -1;
}

/// 最近一次 `kcore_heap_alloc` 的入参快照（`reset_script` 后为 `None`）。
pub(crate) fn last_heap_alloc() -> Option<HeapAllocRecord> {
    *LAST_HEAP_ALLOC.lock().unwrap()
}

/// 最近一次 `kcore_heap_dealloc` 的入参快照（`reset_script` 后为 `None`）。
pub(crate) fn last_heap_dealloc() -> Option<HeapDeallocRecord> {
    *LAST_HEAP_DEALLOC.lock().unwrap()
}

/// 设置下一次 `kcore_memory_acquire` 的回复：`status != 0` 时失败；成功时写
/// `MemoryView { kind = LOCAL_VA, reserved = 0, base, len }`。
pub(crate) fn script_mem_acquire(status: i32, base: usize, len: usize) {
    *MEM_ACQUIRE_SCRIPT.lock().unwrap() = (status, base, len);
}

/// 设置下一次 `kcore_memory_release` 的返回状态。
pub(crate) fn script_mem_release(status: i32) {
    *MEM_RELEASE_SCRIPT.lock().unwrap() = status;
}

/// 设置下一次 `kcore_endpoint_publish` 的返回状态（`0` = staged 成功）。
pub(crate) fn script_publish(status: i32) {
    *PUBLISH_SCRIPT.lock().unwrap() = status;
}

/// 最近一次 publish 的快照（`reset_script` 后为 `None`）。
pub(crate) fn last_publish() -> Option<PublishRecord> {
    LAST_PUBLISH.lock().unwrap().clone()
}

fn copy_region(ptr: *const u8, len: usize) -> Vec<u8> {
    if len == 0 {
        Vec::new()
    } else {
        // SAFETY: 测试替身假设调用方（SDK 自己）传的是本进程内有效切片。
        unsafe { core::slice::from_raw_parts(ptr, len) }.to_vec()
    }
}

/// Core `kcore_endpoint_validate` 的替身：contract + abi 与**已知契约**
/// （block.device / filesystem / probe.result）一致且 id != 0 → 0；否則
/// -ENOENT / -EINVAL（与 Core 档位一致）。
#[unsafe(no_mangle)]
pub extern "C" fn kcore_endpoint_validate(id: u64, contract: u64, abi: u64) -> i32 {
    use crate::generated::block::{KCOMP_BLOCK_DEVICE_ABI, KCOMP_BLOCK_DEVICE_CONTRACT};
    use crate::generated::filesystem::{KCOMP_FILESYSTEM_ABI, KCOMP_FILESYSTEM_CONTRACT};
    use crate::generated::probe::{KCOMP_PROBE_RESULT_ABI, KCOMP_PROBE_RESULT_CONTRACT};
    if id == 0 {
        return -2; // ENOENT
    }
    let expected = if contract == KCOMP_BLOCK_DEVICE_CONTRACT {
        KCOMP_BLOCK_DEVICE_ABI
    } else if contract == KCOMP_FILESYSTEM_CONTRACT {
        KCOMP_FILESYSTEM_ABI
    } else if contract == KCOMP_PROBE_RESULT_CONTRACT {
        KCOMP_PROBE_RESULT_ABI
    } else {
        return -22; // EINVAL
    };
    if abi != expected {
        return -22; // EINVAL
    }
    0
}

/// Core `kcore_endpoint_lookup` 的替身：contract 是已知契约且 provider != 0 →
/// 写入可预测的 id（`provider * 100 + name_len`）；否则 -Errno。
#[unsafe(no_mangle)]
pub extern "C" fn kcore_endpoint_lookup(
    provider: u32,
    _port_name: *const u8,
    port_name_len: usize,
    contract: u64,
    out_endpoint: *mut u64,
) -> i32 {
    use crate::generated::block::KCOMP_BLOCK_DEVICE_CONTRACT;
    use crate::generated::filesystem::KCOMP_FILESYSTEM_CONTRACT;
    use crate::generated::probe::KCOMP_PROBE_RESULT_CONTRACT;
    if out_endpoint.is_null() {
        return -14; // EFAULT
    }
    if contract != KCOMP_BLOCK_DEVICE_CONTRACT
        && contract != KCOMP_FILESYSTEM_CONTRACT
        && contract != KCOMP_PROBE_RESULT_CONTRACT
    {
        return -22; // EINVAL
    }
    if provider == 0 {
        return -2; // ENOENT
    }
    // SAFETY: out 非空（上面已查）；写一个测试可预测的值。
    unsafe {
        core::ptr::write_unaligned(out_endpoint, provider as u64 * 100 + port_name_len as u64)
    };
    0
}

/// Core `kcore_endpoint_publish` 的替身：按脚本返回状态并记录入参。
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn kcore_endpoint_publish(
    port_name: *const u8,
    port_name_len: usize,
    contract: u64,
    kind: u32,
    abi: u64,
    port: u32,
    api: *const (),
    ctx: *mut (),
) -> i32 {
    *LAST_PUBLISH.lock().unwrap() = Some(PublishRecord {
        port_name: copy_region(port_name, port_name_len),
        contract,
        kind,
        abi,
        port,
        api: api as usize,
        ctx: ctx as usize,
    });
    *PUBLISH_SCRIPT.lock().unwrap()
}

/// Core `kcore_memory_acquire` 的替身：按脚本回复 `(status, base, len)`；成功时写
/// `MemoryView`（kind = LOCAL_VA），失败 = `-Errno` 且不写 out（与 Core 同档）。
#[unsafe(no_mangle)]
pub extern "C" fn kcore_memory_acquire(
    _min_len: u64,
    _min_align: u64,
    out_view: *mut crate::abi::MemoryView,
) -> i32 {
    use crate::abi::{KCORE_MEMORY_VIEW_LOCAL_VA, MemoryView};
    if out_view.is_null() {
        return -14; // EFAULT
    }
    let (status, base, len) = *MEM_ACQUIRE_SCRIPT.lock().unwrap();
    if status != 0 {
        return status;
    }
    // SAFETY: out_view 非空（上面已查）；调用方（SDK / 测试）保证可写。
    unsafe {
        out_view.write(MemoryView {
            kind: KCORE_MEMORY_VIEW_LOCAL_VA,
            reserved: 0,
            base: base as u64,
            len: len as u64,
        });
    }
    0
}

/// Core `kcore_memory_release` 的替身：按脚本返回状态；null view → `-EFAULT`。
#[unsafe(no_mangle)]
pub extern "C" fn kcore_memory_release(view: *const crate::abi::MemoryView) -> i32 {
    if view.is_null() {
        return -14; // EFAULT
    }
    *MEM_RELEASE_SCRIPT.lock().unwrap()
}

/// Core `kcore_heap_alloc` 的替身：记录入参；脚本触发时返回 null，否则用 host 的
/// `std::alloc` 按真实 layout 分配（行为与 Core 共享堆/Layout 路由同形）。
#[unsafe(no_mangle)]
pub extern "C" fn kcore_heap_alloc(size: usize, align: usize) -> *mut u8 {
    *LAST_HEAP_ALLOC.lock().unwrap() = Some(HeapAllocRecord { size, align });
    if *HEAP_ALLOC_SCRIPT.lock().unwrap() != 0 || size == 0 {
        return core::ptr::null_mut();
    }
    let Ok(layout) = core::alloc::Layout::from_size_align(size, align) else {
        return core::ptr::null_mut();
    };
    // SAFETY: layout 合法（from_size_align 已校验收 size > 0、align 为 2 的幂）。
    unsafe { std::alloc::alloc(layout) }
}

/// Core `kcore_heap_dealloc` 的替身：记录入参并按同一 layout 归还；null → `EFAULT`，
/// 非法 / `size == 0` layout → `EINVAL`（与 Core 档位一致）。
#[unsafe(no_mangle)]
pub extern "C" fn kcore_heap_dealloc(ptr: *mut u8, size: usize, align: usize) -> i32 {
    *LAST_HEAP_DEALLOC.lock().unwrap() = Some(HeapDeallocRecord {
        ptr: ptr as usize,
        size,
        align,
    });
    if ptr.is_null() {
        return -14; // EFAULT
    }
    if size == 0 {
        return -22; // EINVAL
    }
    let Ok(layout) = core::alloc::Layout::from_size_align(size, align) else {
        return -22; // EINVAL
    };
    // SAFETY: 调用方（adapter / 测试）保证 ptr 来自同 layout 的一次成功 alloc。
    unsafe { std::alloc::dealloc(ptr, layout) };
    0
}

// Unused historical transports are not mocked as business implementations.
#[unsafe(no_mangle)]
pub extern "C" fn kcore_endpoint_bind(
    _: u64,
    _: u64,
    _: u64,
    _: *mut u32,
    _: *mut usize,
    _: *mut usize,
) -> i32 {
    crate::Errno::ENOTSUP.code()
}
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn kcore_endpoint_call(
    _: u64,
    _: u32,
    _: *const u8,
    _: usize,
    _: *const u8,
    _: usize,
    _: *mut u8,
    _: usize,
    _: *mut i32,
) -> i32 {
    crate::Errno::ENOTSUP.code()
}
