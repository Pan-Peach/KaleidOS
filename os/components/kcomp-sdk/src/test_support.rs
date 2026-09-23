//! host 测试的 Core ABI 替身（`#[cfg(test)]` 限定）。
//!
//! kcomp-sdk 的 host 测试无法链接真实 Core；这里用**同一测试二进制内的符号定义**
//! 满足 `generated::abi` 的 extern 声明：
//!
//! - `kcore_endpoint_validate` / `kcore_endpoint_lookup`：行为只由参数决定
//!   （无共享状态），任何测试可并发调用；
//! - `kcore_endpoint_call` / `kcore_endpoint_bind`：行为由脚本变量决定、并记录最近
//!   一次调用——使用它们的测试必须持有 [`lock`]（串行化），避免互相踩。
//!
//! 这些替身**只存在于测试构建**；组件镜像链接的是真实 Core 导出。

use std::sync::Mutex;
use std::vec::Vec;

static CALL_SCRIPT: Mutex<(i32, i32)> = Mutex::new((0, 0));
/// 下一次 `kcore_endpoint_call` 成功时要写进 output 的脚本回复（消费一次）。
static CALL_REPLY: Mutex<Option<Vec<u8>>> = Mutex::new(None);
static LAST_CALL: Mutex<Option<CallRecord>> = Mutex::new(None);
/// `kcore_endpoint_bind` 的脚本回复：`(status, mechanism, api, ctx)`。
static BIND_SCRIPT: Mutex<(i32, u32, usize, usize)> = Mutex::new((0, 0, 0, 0));
static LAST_BIND: Mutex<Option<BindRecord>> = Mutex::new(None);
/// `kcore_endpoint_publish` 的脚本回复（status）与最近一次入参快照。
static PUBLISH_SCRIPT: Mutex<i32> = Mutex::new(0);
static LAST_PUBLISH: Mutex<Option<PublishRecord>> = Mutex::new(None);
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// 最近一次 `kcore_endpoint_call` 的入参快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallRecord {
    pub endpoint: u64,
    pub method: u32,
    pub args: Vec<u8>,
    pub input: Vec<u8>,
    pub output_len: usize,
}

/// 最近一次 `kcore_endpoint_bind` 的入参快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BindRecord {
    pub endpoint: u64,
    pub contract: u64,
    pub abi: u64,
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
    *CALL_SCRIPT.lock().unwrap() = (0, 0);
    *CALL_REPLY.lock().unwrap() = None;
    *LAST_CALL.lock().unwrap() = None;
    *BIND_SCRIPT.lock().unwrap() = (0, 0, 0, 0);
    *LAST_BIND.lock().unwrap() = None;
    *PUBLISH_SCRIPT.lock().unwrap() = 0;
    *LAST_PUBLISH.lock().unwrap() = None;
}

/// 设置下一次 `kcore_endpoint_call` 的 `(transport, method)` 返回。
pub(crate) fn script_call(transport: i32, method: i32) {
    *CALL_SCRIPT.lock().unwrap() = (transport, method);
}

/// 设置下一次成功调用要写进 `output` 的字节（不足补零、超出截断，消费一次）。
pub(crate) fn script_call_reply(reply: &[u8]) {
    *CALL_REPLY.lock().unwrap() = Some(reply.to_vec());
}

/// 设置下一次 `kcore_endpoint_bind` 的成功回复（`status = 0`）。
pub(crate) fn script_bind(mechanism: u32, api: usize, ctx: usize) {
    *BIND_SCRIPT.lock().unwrap() = (0, mechanism, api, ctx);
}

/// 设置下一次 `kcore_endpoint_bind` 的失败回复（`-Errno`）。
pub(crate) fn script_bind_error(status: i32) {
    *BIND_SCRIPT.lock().unwrap() = (status, 0, 0, 0);
}

/// 最近一次调用的快照（`reset_script` 后为 `None`）。
pub(crate) fn last_call() -> Option<CallRecord> {
    LAST_CALL.lock().unwrap().clone()
}

/// 最近一次 bind 的快照（`reset_script` 后为 `None`）。
pub(crate) fn last_bind() -> Option<BindRecord> {
    LAST_BIND.lock().unwrap().clone()
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

/// Core `kcore_endpoint_bind` 的替身：按脚本返回 `(status, mechanism, api, ctx)`；
/// 成功时写 `*out_mechanism`（GATE 不写 api/ctx，与 Core 契约一致），并记录入参。
#[unsafe(no_mangle)]
pub extern "C" fn kcore_endpoint_bind(
    endpoint: u64,
    contract: u64,
    abi: u64,
    out_mechanism: *mut u32,
    out_api: *mut usize,
    out_ctx: *mut usize,
) -> i32 {
    use crate::generated::abi::KCORE_ENDPOINT_MECHANISM_GATE;
    let (status, mechanism, api, ctx) = *BIND_SCRIPT.lock().unwrap();
    *LAST_BIND.lock().unwrap() = Some(BindRecord {
        endpoint,
        contract,
        abi,
    });
    if out_mechanism.is_null() || out_api.is_null() || out_ctx.is_null() {
        return -14; // EFAULT
    }
    if status != 0 {
        return status;
    }
    // SAFETY: 三个 out 在上面已校验非空；调用方（SDK）保证可写。
    unsafe {
        core::ptr::write_unaligned(out_mechanism, mechanism);
        if mechanism != KCORE_ENDPOINT_MECHANISM_GATE {
            core::ptr::write_unaligned(out_api, api);
            core::ptr::write_unaligned(out_ctx, ctx);
        }
    }
    0
}

/// Core `kcore_endpoint_call` 的替身：按脚本返回传输状态；transport == 0 时把
/// method status 写入 `*out_status`，并按脚本把回复写进 `output`（没有脚本回复时，
/// block capacity 调用回填固定值——那是 block 测试既有的契约）。
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn kcore_endpoint_call(
    endpoint: u64,
    method: u32,
    args: *const u8,
    args_len: usize,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: usize,
    out_status: *mut i32,
) -> i32 {
    use crate::generated::block::{KCOMP_BLOCK_CAPACITY_LEN, KCOMP_BLOCK_METHOD_CAPACITY};
    let (transport, status) = *CALL_SCRIPT.lock().unwrap();
    *LAST_CALL.lock().unwrap() = Some(CallRecord {
        endpoint,
        method,
        args: copy_region(args, args_len),
        input: copy_region(input, input_len),
        output_len,
    });
    if transport == 0 && status == 0 && !output.is_null() {
        let scripted = CALL_REPLY.lock().unwrap().take();
        if let Some(reply) = scripted {
            // 截断到 output 容量（与 Core 的窗口语义一致）。
            let len = reply.len().min(output_len);
            // SAFETY: output 非空、调用方保证 output_len 字节可写。
            unsafe { core::ptr::copy_nonoverlapping(reply.as_ptr(), output, len) };
        } else if method == KCOMP_BLOCK_METHOD_CAPACITY && output_len == KCOMP_BLOCK_CAPACITY_LEN {
            // 模拟 provider 回填 capacity = 0x0102_0304_0506_0708（LE）。
            let reply = 0x0102_0304_0506_0708u64.to_le_bytes();
            // SAFETY: 输出窗口由调用方保证长度 = CAPACITY_LEN 且可写。
            unsafe {
                core::ptr::copy_nonoverlapping(reply.as_ptr(), output, KCOMP_BLOCK_CAPACITY_LEN)
            };
        }
    }
    if transport == 0 && !out_status.is_null() {
        // SAFETY: out_status 非空；调用方保证可写。
        unsafe { core::ptr::write_unaligned(out_status, status) };
    }
    transport
}
