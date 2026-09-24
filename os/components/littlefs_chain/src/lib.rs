//! littlefs_chain —— **多实例组合策略**：证明 Core 的 endpoint / instance 模型
//! 能同时承载**两个同类型 FS 实例**，各自独立存储、互不干扰。
//!
//! 每条链（×2）：
//!
//! 1. 创建 provider 实例（`ram_blk_rw`，**可写、per-instance** RAM 块设备，
//!    create config 为空——它自己发布 `block.device` endpoint）；
//! 2. 组合期发现：把 `(provider, port_name, contract)` 解析成 opaque `EndpointId`；
//! 3. 创建 C consumer（`littlefs`），create config **只带 block EndpointId**：
//!    littlefs 自己 `kcomp_block_bind`（Core 在 bind 时选定机制），**不做**全局
//!    名字发现；
//! 4. littlefs 在 create 里 format+mount 自己的块设备、跑内部 selftest，并发布
//!    **filesystem endpoint**；组合策略再次发现它。
//!
//! 两条链用**不同的 provider 实例**（各自 `kcore_memory_acquire` 的 RAM 缓冲），因此
//! 两个 littlefs 挂载在**互相独立**的存储上——这就是"Core 面对多实例"的端到端证据。
//!
//! 本组件**不选择调用机制**：机制由 Core 在 consumer 的 bind 时按两端执行域选定。
//!
//! # QEMU 证据（`tests/qemu/runner.py`）
//!
//! 加载本组件后断言：
//! - `littlefs_chain: chain #0 wired (...)` 与 `chain #1 wired (...)` 各一条，且
//!   两条链的 `provider` / `block_endpoint` / `littlefs` / `fs_endpoint` 各不相同；
//! - 每个 littlefs 各有一条 `[littlefs] mount` / `[littlefs] selftest ok`；
//! - 两个 `ram_blk_rw` 实例各自有独立的 `read` / `write` 业务日志。

#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：提供裸机 #[panic_handler] 等。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

use kcomp_sdk::abi;
use kcomp_sdk::block::{BLOCK_DEVICE_NAME, BlockDevice};
use kcomp_sdk::call;
use kcomp_sdk::endpoint::Endpoint;
use kcomp_sdk::errno::Errno;
use kcomp_sdk::filesystem::{FILESYSTEM_NAME, FileSystem};
use kcomp_sdk::generated::filesystem::KCOMP_FILESYSTEM_METHOD_MOUNT;
use kcomp_sdk::{kcomp_instance_create, kcomp_instance_destroy, klog};

/// 组合策略创建的组件镜像名（= `os/components` 下的目录名，`load` 用同名）。
const PROVIDER_IMAGE: &[u8] = b"ram_blk_rw";
const LITTLEFS_IMAGE: &[u8] = b"littlefs";

/// `littlefs` 的 create config：只交付组合期解析出的 opaque `EndpointId`。
///
/// 布局必须与 `os/components/filesystems/littlefs/littlefs.c` 的
/// `struct littlefs_create_config` 逐字节一致；`config_abi` 是布局指纹（8 字节
/// ASCII "LITTLECS" 的大端读数），对不上由 consumer 拒绝创建。
#[repr(C)]
struct LittlefsCreateConfig {
    endpoint: u64,
}

const LITTLEFS_CREATE_CONFIG_ABI: u64 = 0x4C49_5454_4C45_4353;

/// 多实例条数。
const CHAINS: u32 = 2;

/// 无 config 负载的 create args（`ram_blk_rw` 不需要配置）。
fn empty_args() -> abi::KcompCreateArgs {
    abi::KcompCreateArgs {
        config_abi: 0,
        config: core::ptr::null(),
        config_len: 0,
    }
}

kcomp_instance_create!(|_args, _out_state| {
    let mut chain = 0u32;
    while chain < CHAINS {
        // (1) provider：可写 RAM 块设备，在自己的 create 里发布 block endpoint。
        let mut provider = 0u32;
        let status = unsafe {
            abi::kcore_component_create(
                PROVIDER_IMAGE.as_ptr(),
                PROVIDER_IMAGE.len(),
                &empty_args(),
                &mut provider,
            )
        };
        if status != 0 {
            klog!("littlefs_chain: create ram_blk_rw #{} failed: {}", chain, status);
            return status;
        }

        // (2) 组合期发现：Core 校验 contract + abi + 存活后交付 opaque EndpointId。
        let endpoint = match Endpoint::<BlockDevice>::lookup(provider, BLOCK_DEVICE_NAME) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                klog!("littlefs_chain: block lookup #{} failed: {:?}", chain, error);
                return error.code();
            }
        };

        // (3) C consumer：create config 只带 block EndpointId——littlefs 用
        //     `kcomp_block_bind` 绑定（Core 选定机制），之后经统一包装读写盘。
        let config = LittlefsCreateConfig {
            endpoint: endpoint.id(),
        };
        let args = abi::KcompCreateArgs {
            config_abi: LITTLEFS_CREATE_CONFIG_ABI,
            config: (&config as *const LittlefsCreateConfig).cast(),
            config_len: core::mem::size_of::<LittlefsCreateConfig>(),
        };
        let mut fs = 0u32;
        let status = unsafe {
            abi::kcore_component_create(
                LITTLEFS_IMAGE.as_ptr(),
                LITTLEFS_IMAGE.len(),
                &args,
                &mut fs,
            )
        };
        if status != 0 {
            klog!("littlefs_chain: create littlefs #{} failed: {}", chain, status);
            return status;
        }

        // (4) filesystem endpoint 组合期发现：littlefs 在 create 里发布。
        let fs_endpoint = match Endpoint::<FileSystem>::lookup(fs, FILESYSTEM_NAME) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                klog!("littlefs_chain: fs lookup #{} failed: {:?}", chain, error);
                return error.code();
            }
        };

        // (5) mount：触发该实例的 littlefs format + mount + 自检——只有挂载成功，
        //     本实例才有可读内容，也才证明**本实例的块通路**（prog/erase/read）
        //     真实可用。mount 是 filesystem 契约的方法，经 Core call gate 调用。
        match call::endpoint_call(
            fs_endpoint.id(),
            KCOMP_FILESYSTEM_METHOD_MOUNT,
            &[],
            &[],
            &mut [],
        ) {
            Ok(0) => {}
            other => {
                klog!("littlefs_chain: mount #{} failed: {:?}", chain, other);
                return Errno::EIO.code();
            }
        }

        klog!(
            "littlefs_chain: chain #{} wired (provider={} block_endpoint={} littlefs={} fs_endpoint={})",
            chain,
            provider,
            endpoint.id(),
            fs,
            fs_endpoint.id()
        );

        chain += 1;
    }

    // 无状态组合器：`*out_state` 保持 Core 初始化的 NULL。
    0
});

kcomp_instance_destroy!(|_state| {
    // 组合器无状态、不持有 provider/consumer 的引用：销毁只做日志。
    klog!("littlefs_chain: destroy");
    0
});
