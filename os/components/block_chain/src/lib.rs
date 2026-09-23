//! block_chain —— **第一条完整链的组合策略**（composer）：Rust BlockDevice
//! provider → C consumer（FatFs 胶水）。
//!
//! 它只做组合策略该做的四件事（`docs/architecture/deployment.md` §2 ①）：
//!
//! 1. 创建 provider 实例（`ram_blk`，create config 为空——它自己发布 endpoint）；
//! 2. **组合期发现**：把 `(provider, port_name, contract)` 解析成 opaque
//!    `EndpointId`（Core 校验 contract + abi + 存活）；
//! 3. Gate 探针（诊断）：显式经 `kcore_endpoint_call` 调一次 capacity，证明
//!    provider 的 `kcomp_service_dispatch` 端到端可用——**不**影响消费者绑定；
//! 4. 创建 C consumer（`fatfs`），create config **只带 EndpointId**：consumer
//!    自己 `kcomp_block_bind`（Core 在 bind 时选定机制），**绝不做全局名字发现**。
//!
//! 本组件**不选择调用机制**：机制由 Core 在 consumer 的 bind 时按两端执行域选定。
//!
//! # QEMU 证据
//!
//! `tests/qemu/runner.py` 加载本组件后断言：provider 的 `read` 业务日志 + FatFs
//! 读到的 `HELLO.TXT` 内容逐字节一致，且**业务 read 不伴随任何 gate dispatch**
//! （唯一一条 gate dispatch 是第 3 步的探针，method = capacity）。

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
use kcomp_sdk::generated::block::{KCOMP_BLOCK_CAPACITY_LEN, KCOMP_BLOCK_METHOD_CAPACITY};
use kcomp_sdk::{kcomp_instance_create, kcomp_instance_destroy, klog};

/// 组合策略创建的两个组件镜像名（= `os/components` 下的目录名，`load` 用同名）。
const PROVIDER_IMAGE: &[u8] = b"ram_blk";
const CONSUMER_IMAGE: &[u8] = b"fatfs";

/// `fatfs` 的 create config：只交付组合期解析出的 opaque `EndpointId`。
///
/// 布局必须与 `os/components/filesystems/fatfs/fatfs.c` 的
/// `struct fatfs_create_config` 逐字节一致；`config_abi` 是布局指纹（8 字节 ASCII
/// "FATFSCFG" 的大端读数），对不上由 consumer 拒绝创建。
#[repr(C)]
struct FatfsCreateConfig {
    endpoint: u64,
}

const FATFS_CREATE_CONFIG_ABI: u64 = 0x4641_5446_5343_4647;

/// 无 config 负载的 create args（`ram_blk` 不需要配置）。
fn empty_args() -> abi::KcompCreateArgs {
    abi::KcompCreateArgs {
        config_abi: 0,
        config: core::ptr::null(),
        config_len: 0,
    }
}

kcomp_instance_create!(|_args, _out_state| {
    // (1) provider：ram_blk 在自己的 create 里发布 endpoint（port name = 契约名，
    //     同时交付 Direct 的 api/ctx 与 Gate 的 port token）。
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
        klog!("block_chain: create ram_blk failed: {}", status);
        return status;
    }

    // (2) 组合期发现：Core 校验 contract + abi + 存活后交付 opaque EndpointId。
    //     consumer **不做**这一步（它只从 create config 拿 id）。
    let endpoint = match Endpoint::<BlockDevice>::lookup(provider, BLOCK_DEVICE_NAME) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            klog!("block_chain: endpoint lookup failed: {:?}", error);
            return error.code();
        }
    };

    // (3) Gate 探针：显式经 Core call gate 调一次 capacity。它证明 provider 的
    //     `kcomp_service_dispatch` 真实可用（host 测试无法证明真实 stack switch）；
    //     消费者的绑定与它无关（机制由 Core 在 consumer bind 时另行选定）。
    let mut reply = [0u8; KCOMP_BLOCK_CAPACITY_LEN];
    match call::endpoint_call(
        endpoint.id(),
        KCOMP_BLOCK_METHOD_CAPACITY,
        &[],
        &[],
        &mut reply,
    ) {
        Ok(0) => {}
        other => {
            klog!("block_chain: gate probe failed: {:?}", other);
            return Errno::EIO.code();
        }
    }
    klog!(
        "block_chain: gate probe ok (sectors={})",
        u64::from_le_bytes(reply)
    );

    // (4) C consumer：create config 只带 EndpointId——FatFs 用 `kcomp_block_bind`
    //     绑定（Core 选定机制），之后经统一包装读盘。
    let config = FatfsCreateConfig {
        endpoint: endpoint.id(),
    };
    let consumer_args = abi::KcompCreateArgs {
        config_abi: FATFS_CREATE_CONFIG_ABI,
        config: (&config as *const FatfsCreateConfig).cast(),
        config_len: core::mem::size_of::<FatfsCreateConfig>(),
    };
    let mut consumer = 0u32;
    let status = unsafe {
        abi::kcore_component_create(
            CONSUMER_IMAGE.as_ptr(),
            CONSUMER_IMAGE.len(),
            &consumer_args,
            &mut consumer,
        )
    };
    if status != 0 {
        klog!("block_chain: create fatfs failed: {}", status);
        return status;
    }

    klog!(
        "block_chain: chain wired (provider={} endpoint={} consumer={})",
        provider,
        endpoint.id(),
        consumer
    );
    // 无状态组合器：`*out_state` 保持 Core 初始化的 NULL。
    0
});

kcomp_instance_destroy!(|_state| {
    // 组合器无状态、不持有 provider/consumer 的引用：销毁只做日志。
    klog!("block_chain: destroy");
    0
});
