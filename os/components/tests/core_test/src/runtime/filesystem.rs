//! 文件系统集成场景：CoreTest 以 SDK filesystem client 直接扮演消费者。
//!
//! - **block chain**：`ram_blk`（只读 FAT12 合成 provider）→ 组合期解析
//!   `block.device` endpoint → `fatfs`（create config 只带 EndpointId）→
//!   `filesystem` endpoint。CoreTest 显式经 Core call gate 各探针一次（证明
//!   provider 的 `kcomp_service_dispatch` 真实可用），再 `bind` + `mount` /
//!   `open` / `read` / `close` / `unmount` 读 `0:/HELLO.TXT` 并**逐字节**比对。
//! - **littlefs multi-instance**：2×（`ram_blk_rw` → `littlefs`），两个独立
//!   provider、两个独立 `EndpointId`、两个独立文件系统实例；各自 mount（内部
//!   format + selftest）并从自己的 filesystem endpoint 读回 selftest 文件。
//! - **littlefs isolation**：把实例 A 的**原始存储**整段擦成 0xFF 后，A 的
//!   selftest 文件不再读得出，而 B 的实例仍逐字节正确——两个实例的状态不共享。
//! - **component multi-instance（block 级直证）**：同一 `ram_blk_rw` artifact 的
//!   两个实例，各自对同一扇区写不同 pattern——A 的写不出现在 B 的读回里、B 的写
//!   不影响 A，不经文件系统直接证明 per-instance backing 不共享（身份断言走 trace）。
//!
//! 全部在 task context 执行（块调用契约要求 task；消费者必须是 task）。task 只把
//! 结果写回 [`State`]，报告在 create 上下文里、调度返回后统一发出。
//!
//! 机制证据：组件内看不到 provider 的日志，因此读 trace 的 `EndpointBind` 事件
//! ——业务绑定必须是 **Direct**（[`trace::MECHANISM_DIRECT`]），显式探针不产生
//! bind 事件。

use core::ffi::CStr;

use kcomp_sdk::abi::{self, KcompCreateArgs};
use kcomp_sdk::block::{BLOCK_DEVICE_NAME, BlockDevice};
use kcomp_sdk::call;
use kcomp_sdk::endpoint::Endpoint;
use kcomp_sdk::filesystem::client::FileSystemBinding;
use kcomp_sdk::filesystem::{FILESYSTEM_NAME, FILESYSTEM_OPEN_READ, FileSystem};
use kcomp_sdk::generated::block::{KCOMP_BLOCK_CAPACITY_LEN, KCOMP_BLOCK_METHOD_CAPACITY};
use kcomp_sdk::generated::filesystem::{
    KCOMP_FILESYSTEM_METHOD_MOUNT, KCOMP_FILESYSTEM_READ_HEADER_LEN,
};
use kcomp_sdk::klog;

use super::report::Checks;
use super::trace;

/// 组件镜像名（= `os/components` 下的目录名）。
const BLOCK_PROVIDER: &[u8] = b"ram_blk";
const BLOCK_PROVIDER_RW: &[u8] = b"ram_blk_rw";
const FATFS: &[u8] = b"fatfs";
const LITTLEFS: &[u8] = b"littlefs";

/// 组合策略交付的 create config：只带组合期解析出的 opaque `EndpointId`。
///
/// 布局必须与 `fatfs.c` / `littlefs.c` 的 `struct *_create_config` 逐字节一致；
/// `config_abi` 是布局指纹（8 字节 ASCII 的大端读数），对不上由 consumer 拒绝创建。
#[repr(C)]
struct EndpointCreateConfig {
    endpoint: u64,
}

const FATFS_CREATE_CONFIG_ABI: u64 = 0x4641_5446_5343_4647; // "FATFSCFG"
const LITTLEFS_CREATE_CONFIG_ABI: u64 = 0x4C49_5454_4C45_4353; // "LITTLECS"

/// `ram_blk` 的合成 FAT12 卷里 `HELLO.TXT` 的内容（与 provider 的 `fat12.rs` 一致）。
const HELLO_PATH: &CStr = c"0:/HELLO.TXT";
const HELLO_CONTENT: &[u8] = b"KALEIDOS BLOCK CHAIN OK";

/// littlefs mount 自检写入的文件（与 `littlefs_backend.c` 的 selftest 一致）。
const SELFTEST_PATH: &CStr = c"selftest.txt";
const SELFTEST_CONTENT: &[u8] = b"KaleidOS littlefs selftest: prog/erase/read ok";

/// 多实例链数。
const CHAINS: usize = 2;

/// block.device 的 sector 大小（写 A 的原始存储用）。
const SECTOR: usize = 512;

/// 本场景组的结果（task 写、create 报告；未跑到 = false）。
#[repr(C)]
pub struct State {
    /// block chain 全链成功（含精确内容）。
    pub block_chain: bool,
    /// block chain 的业务绑定机制是 Direct（block + filesystem）。
    pub block_chain_direct: bool,
    /// littlefs 两条链都挂载成功、selftest 文件都读得出、身份两两不同。
    pub littlefs_multi: bool,
    /// littlefs 两实例的存储互不相干。
    pub littlefs_isolation: bool,
    /// littlefs 业务绑定机制是 Direct（block + filesystem）。
    pub littlefs_direct: bool,
    /// 同一 `ram_blk_rw` artifact 的两个实例（block 级）存储互不相干。
    pub component_multi_instance: bool,
}

/// 无 config 负载的 create args（`ram_blk` / `ram_blk_rw` 不需要配置）。
const fn empty_args() -> KcompCreateArgs {
    KcompCreateArgs {
        config_abi: 0,
        config: core::ptr::null(),
        config_len: 0,
    }
}

/// 创建无配置实例；成功返回 instance id。
fn create(image: &[u8]) -> Option<u32> {
    let mut instance = 0u32;
    (unsafe {
        abi::kcore_component_create(image.as_ptr(), image.len(), &empty_args(), &mut instance)
    } == 0)
        .then_some(instance)
}

/// 创建“只带 EndpointId”的实例（config 布局与 ABI 指纹由调用方给出）。
fn create_with_endpoint(image: &[u8], config_abi: u64, endpoint: u64) -> Option<u32> {
    let config = EndpointCreateConfig { endpoint };
    // FatFs must decode opaque bytes even when the payload is not u64-aligned.
    #[repr(align(8))]
    struct UnalignedConfig([u8; 9]);
    let mut bytes = UnalignedConfig([0; 9]);
    bytes.0[1..].copy_from_slice(&endpoint.to_ne_bytes());
    let args = KcompCreateArgs {
        config_abi,
        config: if image == FATFS {
            bytes.0[1..].as_ptr().cast()
        } else {
            (&config as *const EndpointCreateConfig).cast()
        },
        config_len: core::mem::size_of::<EndpointCreateConfig>(),
    };
    let mut instance = 0u32;
    (unsafe { abi::kcore_component_create(image.as_ptr(), image.len(), &args, &mut instance) } == 0)
        .then_some(instance)
}

/// 经 filesystem 绑定读一个文件并逐字节比对期望内容。`read` 的缓冲前 8 字节是
/// LE 长度头，数据从 offset 8 开始（SDK client 的契约布局）。
fn read_exact(binding: &FileSystemBinding, path: &CStr, expected: &[u8]) -> bool {
    let mut frame = [0u8; KCOMP_FILESYSTEM_READ_HEADER_LEN + 64];
    if expected.len() > 64 {
        return false;
    }
    let Ok(handle) = binding.open(path, FILESYSTEM_OPEN_READ) else {
        return false;
    };
    let read = binding.read(handle, &mut frame);
    let closed = binding.close(handle).is_ok();
    let content_ok = match read {
        Ok(actual) => {
            actual == expected.len()
                && frame
                    [KCOMP_FILESYSTEM_READ_HEADER_LEN..KCOMP_FILESYSTEM_READ_HEADER_LEN + actual]
                    == *expected
        }
        Err(_) => false,
    };
    content_ok && closed
}

/// block 级多实例直证：两个同源 `ram_blk_rw` 实例对同一扇区的读写互不可见。
///
/// 先各自读回原始内容，再向 `a` 写 pattern、断言 `b` 读回仍等于自己的原始内容
/// 且不含 pattern；随后向 `b` 写不同 pattern、断言 `a` 仍持有自己的写入。pattern
/// 刻意与对应实例的原始内容不同（取反码），因此"写没生效 / 内容恰好相同"不可能
/// 蒙混过关。若两实例共享同一 backing，A（或 B）的写会出现在对方的读回里，本
/// 函数必然失败。
fn blocks_independent(a: &Endpoint<BlockDevice>, b: &Endpoint<BlockDevice>) -> bool {
    let (Ok(a), Ok(b)) = (a.bind(), b.bind()) else {
        return false;
    };
    let lba = 0u64;
    let mut a_before = [0u8; SECTOR];
    let mut b_before = [0u8; SECTOR];
    let originals = a.read(lba, &mut a_before).is_ok() && b.read(lba, &mut b_before).is_ok();

    // A 的 pattern 与 A、B 的原始内容都不同（各取一个字节的反码）：写后 A 必须
    // 等于它，而 B 必须仍等于自己的原始内容。
    let mut pattern_a = [0xA5u8; SECTOR];
    pattern_a[0] = b_before[0] ^ 0xFF;
    pattern_a[1] = a_before[1] ^ 0xFF;
    let wrote_a = a.write(lba, &pattern_a).is_ok();
    let mut b_after_a = [0u8; SECTOR];
    let b_untouched =
        b.read(lba, &mut b_after_a).is_ok() && b_after_a == b_before && b_after_a != pattern_a;

    // B 写入不同 pattern：B 自己读回新内容，A 仍持有自己的 pattern。
    let pattern_b = [0x5Au8; SECTOR];
    let wrote_b = b.write(lba, &pattern_b).is_ok();
    let mut b_after_b = [0u8; SECTOR];
    let mut a_after_b = [0u8; SECTOR];
    let b_holds = b.read(lba, &mut b_after_b).is_ok() && b_after_b == pattern_b;
    let a_holds = a.read(lba, &mut a_after_b).is_ok() && a_after_b == pattern_a;

    originals && wrote_a && b_untouched && wrote_b && b_holds && a_holds
}

/// block chain：provider → fatfs → filesystem endpoint → 精确内容。
/// 返回 (全链成功, 绑定机制全 Direct)。
fn block_chain() -> (bool, bool) {
    // (1) provider：ram_blk 在自己的 create 里发布 block endpoint。
    let Some(provider) = create(BLOCK_PROVIDER) else {
        klog!("[core-test] create ram_blk failed");
        return (false, false);
    };
    let Ok(block_endpoint) = Endpoint::<BlockDevice>::lookup(provider, BLOCK_DEVICE_NAME) else {
        klog!("[core-test] block endpoint lookup failed");
        return (false, false);
    };

    // (2) Gate 探针：显式经 Core call gate 调一次 capacity（证明 provider 的
    //     kcomp_service_dispatch 真实可用）。
    let mut capacity = [0u8; KCOMP_BLOCK_CAPACITY_LEN];
    let block_gate = call::endpoint_call(
        block_endpoint.id(),
        KCOMP_BLOCK_METHOD_CAPACITY,
        &[],
        &[],
        &mut capacity,
    ) == Ok(0)
        && u64::from_le_bytes(capacity) > 0;

    // 机制证据窗口：block 绑定（fatfs 内部）与 filesystem 绑定（本处）都在窗口内。
    let window = trace::cursor();

    // (3) fatfs：create config 只带 block EndpointId（它自己 bind，不做名字发现）。
    let Some(consumer) = create_with_endpoint(FATFS, FATFS_CREATE_CONFIG_ABI, block_endpoint.id())
    else {
        klog!("[core-test] create fatfs failed");
        return (false, false);
    };
    let Ok(fs_endpoint) = Endpoint::<FileSystem>::lookup(consumer, FILESYSTEM_NAME) else {
        klog!("[core-test] filesystem endpoint lookup failed");
        return (false, false);
    };

    // (4) filesystem Gate 探针：显式经 Core call gate mount 一次（fatfs mount 幂等）。
    let fs_gate = call::endpoint_call(
        fs_endpoint.id(),
        KCOMP_FILESYSTEM_METHOD_MOUNT,
        &[],
        &[],
        &mut [],
    ) == Ok(0);

    // (5) 消费者角色：bind（Core 选定机制）→ mount → open → read → close → unmount。
    let Ok(binding) = fs_endpoint.bind() else {
        klog!("[core-test] filesystem bind failed");
        return (false, false);
    };
    let content = binding.mount().is_ok()
        && read_exact(&binding, HELLO_PATH, HELLO_CONTENT)
        && binding.unmount().is_ok();

    // (6) 机制证据：两条业务绑定都必须 Direct（旧 runner 的“无 gate dispatch”断言）。
    let block_id = block_endpoint.id();
    let fs_id = fs_endpoint.id();
    let mut block_direct = false;
    let mut fs_direct = false;
    trace::binds(window, |endpoint, mechanism| {
        if mechanism == trace::MECHANISM_DIRECT {
            block_direct |= endpoint == block_id;
            fs_direct |= endpoint == fs_id;
        }
    });

    (
        (block_gate && fs_gate && content),
        (block_direct && fs_direct),
    )
}

/// littlefs 多实例 + 隔离 + block 级多实例直证。
/// 返回 (多实例成功, 存储隔离成立, 绑定机制 Direct, block 级两实例独立)。
fn littlefs_multi() -> (bool, bool, bool, bool) {
    let window = trace::cursor();
    let mut providers = [0u32; CHAINS];
    let mut instances = [0u32; CHAINS];
    let mut block_endpoints: [Option<Endpoint<BlockDevice>>; CHAINS] = [None; CHAINS];
    let mut fs_endpoints: [Option<Endpoint<FileSystem>>; CHAINS] = [None; CHAINS];
    let mut bindings: [Option<FileSystemBinding>; CHAINS] = [const { None }; CHAINS];

    // (1) 两条链：ram_blk_rw → block endpoint → littlefs → filesystem endpoint → bind。
    let mut wired = true;
    for chain in 0..CHAINS {
        let Some(provider) = create(BLOCK_PROVIDER_RW) else {
            wired = false;
            break;
        };
        providers[chain] = provider;
        let Ok(block) = Endpoint::<BlockDevice>::lookup(provider, BLOCK_DEVICE_NAME) else {
            wired = false;
            break;
        };
        block_endpoints[chain] = Some(block);
        let Some(fs) = create_with_endpoint(LITTLEFS, LITTLEFS_CREATE_CONFIG_ABI, block.id())
        else {
            wired = false;
            break;
        };
        instances[chain] = fs;
        let Ok(fs_endpoint) = Endpoint::<FileSystem>::lookup(fs, FILESYSTEM_NAME) else {
            wired = false;
            break;
        };
        fs_endpoints[chain] = Some(fs_endpoint);
        let Ok(binding) = fs_endpoint.bind() else {
            wired = false;
            break;
        };
        bindings[chain] = Some(binding);
    }

    // (2) mount：chain 0 额外经 Core call gate 探针一次（Gate transport 可用），
    //     两条链都经绑定 mount（内部 format + selftest）。
    let mut mounted = wired;
    match fs_endpoints[0] {
        Some(endpoint) => {
            mounted &= call::endpoint_call(
                endpoint.id(),
                KCOMP_FILESYSTEM_METHOD_MOUNT,
                &[],
                &[],
                &mut [],
            ) == Ok(0);
        }
        None => mounted = false,
    }
    for binding in &bindings {
        match binding {
            Some(binding) => mounted &= binding.mount().is_ok(),
            None => mounted = false,
        }
    }

    // (3) 每个实例读回自己的 selftest 文件（各自挂载了自己的独立存储）。
    let mut selftest = mounted;
    for binding in &bindings {
        match binding {
            Some(binding) => selftest &= read_exact(binding, SELFTEST_PATH, SELFTEST_CONTENT),
            None => selftest = false,
        }
    }

    // (4) 身份两两不同：provider / 文件系统实例 / 两类 endpoint。
    let distinct = wired
        && providers[0] != providers[1]
        && instances[0] != instances[1]
        && block_endpoints[0].map(|e| e.id()) != block_endpoints[1].map(|e| e.id())
        && fs_endpoints[0].map(|e| e.id()) != fs_endpoints[1].map(|e| e.id());
    let multi = wired && mounted && selftest && distinct;

    // (5) 机制证据：chain 0 的 block / filesystem 业务绑定必须 Direct。
    let mut block_direct = false;
    let mut fs_direct = false;
    if let (Some(block), Some(fs)) = (block_endpoints[0], fs_endpoints[0]) {
        trace::binds(window, |endpoint, mechanism| {
            if mechanism == trace::MECHANISM_DIRECT {
                block_direct |= endpoint == block.id();
                fs_direct |= endpoint == fs.id();
            }
        });
    }

    // (6) 隔离：把实例 0 的**原始存储**整段擦成 0xFF——文件系统 A 的内容必须
    //     不再读得出，而实例 1 的文件系统必须逐字节不受影响（状态不共享）。
    let isolation = if multi {
        let erased = match block_endpoints[0] {
            Some(raw_endpoint) => match raw_endpoint.bind() {
                Ok(raw) => match raw.capacity_sectors() {
                    Ok(capacity) if capacity > 0 => {
                        let erased_sector = [0xFFu8; SECTOR];
                        let mut ok = true;
                        for lba in 0..capacity {
                            ok &= raw.write(lba, &erased_sector).is_ok();
                        }
                        ok
                    }
                    _ => false,
                },
                Err(_) => false,
            },
            None => false,
        };
        match (&bindings[0], &bindings[1]) {
            (Some(a), Some(b)) => {
                erased
                    && !read_exact(a, SELFTEST_PATH, SELFTEST_CONTENT)
                    && read_exact(b, SELFTEST_PATH, SELFTEST_CONTENT)
            }
            _ => false,
        }
    } else {
        false
    };

    // (7) 组件多实例（block 级直证）：同一 `ram_blk_rw` artifact 的两个实例，身份
    //     互异（trace 里有出生记录、各自走完 Ready 生命周期、block EndpointId 不同）
    //     且扇区存储互不相干。比 littlefs 层更直接：不经文件系统，直接对两个
    //     provider 的同一扇区读写——共享 backing 会让 A 的写出现在 B 的读回里。
    let multi_instance = match (block_endpoints[0], block_endpoints[1]) {
        (Some(a), Some(b)) => {
            let mut declared = [0u32; CHAINS * 2];
            let declared_count = trace::declared_components(window, &mut declared);
            let declared_both = declared[..declared_count].contains(&providers[0])
                && declared[..declared_count].contains(&providers[1]);
            declared_both
                && providers[0] != providers[1]
                && trace::component_lifecycle(window, providers[0] as i32)
                && trace::component_lifecycle(window, providers[1] as i32)
                && a.id() != b.id()
                && blocks_independent(&a, &b)
        }
        _ => false,
    };

    (multi, isolation, block_direct && fs_direct, multi_instance)
}

/// 场景 task：全部文件系统集成动作都在 task context 里跑。
extern "C" fn task(arg: *mut ()) {
    // SAFETY: `arg` 是 create 里写入 `*out_state` 的 State 的 filesystem 字段，
    // 实例存活期间地址稳定；本任务是唯一写者（单 CPU、无并发）。
    let state = unsafe { &mut *(arg as *mut State) };
    let (block_chain_ok, block_chain_direct) = block_chain();
    state.block_chain = block_chain_ok;
    state.block_chain_direct = block_chain_direct;
    let (multi, isolation, direct, multi_instance) = littlefs_multi();
    state.littlefs_multi = multi;
    state.littlefs_isolation = isolation;
    state.littlefs_direct = direct;
    state.component_multi_instance = multi_instance;

    unsafe { kcomp_sdk::abi::kcore_task_exit() };
    // task_exit 永不返回本任务；防御性驻留（不可达）。
    loop {
        core::hint::spin_loop();
    }
}

/// 创建并启动场景 task（`state` 是实例状态里的本组字段）。
pub fn spawn(state: *mut State) {
    let mut task_id = 0u32;
    // SAFETY: entry 在本镜像内；arg = 本组 state；out_task 可写。
    let rc = unsafe { kcomp_sdk::abi::kcore_task_create(task, state.cast(), &mut task_id) };
    if rc != 0 {
        klog!(
            "[core-test] filesystem scenario task create failed (rc={})",
            rc
        );
        return;
    }
    // SAFETY: task_id 由上一行成功创建。
    if unsafe { kcomp_sdk::abi::kcore_task_start(task_id) } != 0 {
        klog!("[core-test] filesystem scenario task start failed");
    }
}

/// 调度返回后报告本组（`state` 由场景 task 填写）。
pub fn report(checks: &mut Checks, state: &State) {
    checks.group("filesystem chain");
    checks.check(30, "block-chain", state.block_chain);
    checks.check(31, "block-chain-direct", state.block_chain_direct);
    checks.check(32, "littlefs-multi-instance", state.littlefs_multi);
    checks.check(33, "littlefs-isolation", state.littlefs_isolation);
    checks.check(
        42,
        "component-multi-instance",
        state.component_multi_instance,
    );
    checks.check(34, "littlefs-direct", state.littlefs_direct);
}
