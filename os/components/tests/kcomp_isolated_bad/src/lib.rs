//! kcomp_isolated_bad —— 失败/重启矩阵的**放段失败**夹具。
//!
//! 它是一份**合法** `.kcomp`（ET_REL、`kcomp_instance_create` / `destroy` /
//! `kcomp_abi` 齐备、UNDEF 空集、重定位白名单内），因此能通过 packer 的四项契约
//! 校验与 Isolated 装载的 import 包络门禁；但它的一个 ALLOC 段
//! （17 MiB 零初始化 `.bss`）**超出按域装载的实例镜像窗口**
//! （`isolated_load::ISOLATED_IMAGE_WINDOW` = 16 MiB），所以
//! `isolated_load::place` 必须在这里显式拒绝（`SegmentOutsideWindow` →
//! `ComponentLoadError::IsolatedPlacementFailed`）。
//!
//! 用途（ArchTest `isolated-load-reject`）：证明**放段阶段的失败**在声明实例 /
//! 创建 AS / 登记 image **之前**就被 Core 拒绝——不留任何半成品，也绝不把
//! "装不进来的镜像"降级成别的域跑。
//!
//! 为什么不塞一个非法 import：那会被更早的 import 包络门禁拒绝
//! （`IsolatedImportUnsupported`），证明的不是放段阶段。本夹具刻意让拒绝点
//! **精确落在放段**。
//!
//! 超大段必须是**被引用**的，否则 `--gc-sections` 会把它丢掉（那夹具就变成
//! 合法镜像了）；`kcomp_instance_create` 里 `black_box` 取它的地址。

#![no_std]

/// 超出实例镜像窗口（16 MiB）的零初始化段：`.bss`（NOBITS）因此 `.kcomp`
/// 文件本身仍然很小，但按域放段规划出来的段区间装不下。
#[used]
static mut BAD_BSS: [u8; 17 * 1024 * 1024] = [0; 17 * 1024 * 1024];

/// 组件 ABI 的必需入口：引用超大段，保证 `--gc-sections` 不丢它。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_instance_create(_args: *const (), _out_state: *mut *mut ()) -> i32 {
    // 只取地址（不解引用）：本夹具的 create 永远不会被真实调用——放段先失败。
    core::hint::black_box(core::ptr::addr_of!(BAD_BSS));
    0
}

/// 组件 ABI 的必需入口（本夹具永远不会被调用）。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_instance_destroy(_state: *mut ()) -> i32 {
    0
}

/// 精确契约指纹（手工锚定，与 `abi/component.toml` 的 `KCOMP_ABI` 同值）。
#[unsafe(no_mangle)]
pub static kcomp_abi: u64 = 0x9D73_405B_B2F8_16C0;

/// 组件私有 panic handler：存在只为满足链接前提，且**刻意不引 `kcore_*`**
/// （空 import 包络——拒绝点必须是放段，不是 import 门禁）。
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
