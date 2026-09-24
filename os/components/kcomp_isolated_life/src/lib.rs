//! kcomp_isolated_life —— increment 5（Isolated 生命周期接线）的 ArchTest
//! **真实 `.kcomp`**：由生产路径 `create_component(name, args, IsolatedNative)`
//! 创建、`stop_component` 销毁，全程跑在**自己的私有 AS** 里。
//!
//! # 零依赖、零 import
//!
//! Isolated 的 import 包络仍是**空集**（本增量选择 Core 预置内存窗口、不引入
//! component→Core 的 gate-call trampoline）：本夹具只用 `core::arch::asm!` 读
//! CSR 与自己镜像内的 load/store，没有任何 UNDEF 符号。
//!
//! # 与 Core 的接口（increment 5 的窗口协议）
//!
//! Core 经 gateway 把 `a0 = args`、`a1 = out_state` 交给 `kcomp_instance_create`：
//!
//! ```text
//! args     → 实例窗口基址 + 0（KcompCreateArgs：config_abi / config / config_len）
//! out_state→ 实例窗口基址 + 32（usize 槽；组件写，Core 从自己的视图读回）
//! tp       = 实例窗口基址 + 64（Core 安装的 per-instance runtime slot）
//! ```
//!
//! 组件在窗口内 `args + REPORT_OFF` 写一份**上报**（magic / 观察到的 tp / satp /
//! args / out_state / config 负载），并把 `*out_state` 指向它。ArchTest 从 Core
//! 侧（窗口 backing 的 Core 视图）读回并断言：组件确实在私有 AS 里跑过、args 与
//! config 真的送到了、`tp` 就是 Core 为该实例安装的 runtime slot。
//!
//! destroy 把 `DESTROY_MAGIC` 写进 `state + DESTROY_OFF`（`state` = create 写回的
//! out_state），ArchTest 用它在销毁后证明 destroy 入口**真的执行过**。
//!
//! # 故障注入
//!
//! `config_abi == FAIL_ABI` 时 create 立刻返回 `-EINVAL`（不写任何槽位）：
//! ArchTest 用它验证"create-entry 失败 → 实例 Failed + AS 退役 + 窗口归还"。

#![no_std]

/// 与 Core 侧 `KcompCreateArgs` 逐字同形（`#[repr(C)]`，Core 视为不透明字节）。
#[repr(C)]
pub struct CreateArgs {
    config_abi: u64,
    config: *const u8,
    config_len: usize,
}

/// 上报区在实例窗口里的偏移（Core 不解释；ArchTest 按同一偏移读回）。
const REPORT_OFF: usize = 512;
/// Core 预交付的域视图（`kcore_memory_view`）在窗口里的偏移（increment 5 布局）。
const WINDOW_VIEW_OFF: usize = 384;
/// destroy 标记相对 `state`（= 上报区基址）的偏移。
const DESTROY_OFF: usize = 10 * core::mem::size_of::<usize>();

const REPORT_MAGIC: usize = 0x4C49_4645; // "LIFE"
const DESTROY_MAGIC: usize = 0x4C49_4644; // "LIFD"
/// 故障注入：create 见到这个 config_abi 就返回 `-EINVAL`（不写任何槽位）。
const FAIL_ABI: u64 = 0xDEAD_BEEF;
/// 故障注入：create 见到这个 config_abi 就执行非法指令（gateway 故障分派）。
const FAULT_ABI: u64 = 0xDEAD_FA11;
const STATUS_INVALID_CONFIG: i32 = -22; // -EINVAL

const R_MAGIC: usize = 0;
const R_TP: usize = 1;
const R_SATP: usize = 2;
const R_ARGS: usize = 3;
const R_OUT_STATE: usize = 4;
const R_CONFIG_ABI: usize = 5;
const R_CONFIG_LEN: usize = 6;
const R_CONFIG0: usize = 7;
const R_CONFIG1: usize = 8;
const R_SELF: usize = 9;
const R_VIEW_KIND: usize = 11;
const R_VIEW_BASE: usize = 12;
const R_VIEW_LEN: usize = 13;

fn read_tp() -> usize {
    let tp: usize;
    // SAFETY: 只读寄存器，无内存 / 栈副作用。
    unsafe {
        core::arch::asm!("mv {tp}, tp", tp = out(reg) tp, options(nomem, nostack, preserves_flags));
    }
    tp
}

fn read_satp() -> usize {
    let satp: usize;
    // SAFETY: 只读 CSR。
    unsafe {
        core::arch::asm!(
            "csrr {satp}, satp",
            satp = out(reg) satp,
            options(nostack, preserves_flags),
        );
    }
    satp
}

/// Core 预交付的域视图（`#[repr(C)]`，与 `kcore_memory_view` 同形）。
#[repr(C)]
struct MemoryViewAbi {
    kind: u32,
    reserved: u32,
    base: u64,
    len: u64,
}

fn report(args: usize, out_state: usize, args_struct: &CreateArgs) -> usize {
    let report = args + REPORT_OFF;
    // SAFETY: Core 在 create 前把域视图写进窗口（+WINDOW_VIEW_OFF，8 字节对齐）；
    // 组件只读。
    let view = unsafe { &*((args + WINDOW_VIEW_OFF) as *const MemoryViewAbi) };
    // SAFETY: report 在实例窗口内（ArchTest 断言窗口布局）；Core 侧 backing 是
    // 有效可写区域，组件在私有 AS 内写自己的窗口。
    unsafe {
        let slots = report as *mut usize;
        slots.add(R_MAGIC).write_volatile(REPORT_MAGIC);
        slots.add(R_TP).write_volatile(read_tp());
        slots.add(R_SATP).write_volatile(read_satp());
        slots.add(R_ARGS).write_volatile(args);
        slots.add(R_OUT_STATE).write_volatile(out_state);
        slots.add(R_CONFIG_ABI).write_volatile(args_struct.config_abi as usize);
        slots.add(R_CONFIG_LEN).write_volatile(args_struct.config_len);
        slots.add(R_CONFIG0).write_volatile(if args_struct.config_len > 0 {
            args_struct.config.read_volatile() as usize
        } else {
            0
        });
        slots.add(R_CONFIG1).write_volatile(if args_struct.config_len > 1 {
            args_struct.config.add(1).read_volatile() as usize
        } else {
            0
        });
        slots.add(R_SELF).write_volatile(kcomp_instance_create as *const () as usize);
        slots.add(R_VIEW_KIND).write_volatile(view.kind as usize);
        slots.add(R_VIEW_BASE).write_volatile(view.base as usize);
        slots.add(R_VIEW_LEN).write_volatile(view.len as usize);
    }
    report
}

/// 组件 ABI 的必需入口：`kcomp_instance_create(const KcompCreateArgs *, void **) -> i32`。
///
/// `allow(not_unsafe_ptr_arg_deref)`：与 SDK 的 `kcomp_instance_create!` 同一纪律
/// ——入口必须是**安全** `extern "C" fn`（ABI 形状），指针有效性是调用方（Core）
/// 的契约，函数内只在已经校验的前提下解引用。
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn kcomp_instance_create(args: *const CreateArgs, out_state: *mut *mut ()) -> i32 {
    if args.is_null() || out_state.is_null() {
        return STATUS_INVALID_CONFIG;
    }
    // SAFETY: Core 传入的 args 指向实例窗口内的 KcompCreateArgs（本函数只读）。
    let args_struct = unsafe { &*args };
    if args_struct.config_abi == FAIL_ABI {
        // 故障注入：不写任何槽位，直接失败。
        return STATUS_INVALID_CONFIG;
    }
    if args_struct.config_abi == FAULT_ABI {
        // 故障注入：在私有 AS 里执行非法指令——Core 的窄故障分派（无显式策略 =
        // 不可恢复）应放弃本组件，create 以 `Faulted` 收场。
        // SAFETY: 故意 trap；本指令之后不会再被执行（组件被放弃）。
        unsafe { core::arch::asm!(".4byte 0", options(nostack)) };
        return STATUS_INVALID_CONFIG;
    }
    let report_ptr = report(args as usize, out_state as usize, args_struct);
    // SAFETY: out_state 指向实例窗口内的 usize 槽（Core 清零、Core 读回）。
    unsafe { out_state.write(report_ptr as *mut ()) };
    0
}

/// 组件 ABI 的必需入口：`kcomp_instance_destroy(void *state) -> i32`。
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn kcomp_instance_destroy(state: *mut ()) -> i32 {
    if state.is_null() {
        return 0;
    }
    // SAFETY: state 是 create 写回的实例窗口内地址（同一实例，仍然映射）。
    unsafe {
        let slot = (state as usize + DESTROY_OFF) as *mut usize;
        slot.write_volatile(DESTROY_MAGIC);
    }
    0
}

/// 精确契约指纹（手工锚定，与 `abi/component.toml` 的 `KCOMP_ABI` 同值）。
#[unsafe(no_mangle)]
pub static kcomp_abi: u64 = 0x4B43_4F4D_5041_4249;

/// 组件私有 panic handler：本夹具没有 panic 源，存在只为满足链接前提，且
/// **刻意不引 `kcore_*`**（空 import 包络）。
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
