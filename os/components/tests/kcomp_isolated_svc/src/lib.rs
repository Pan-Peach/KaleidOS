//! kcomp_isolated_svc —— increment 6（KernelNative caller → Isolated provider 的
//! 跨域 service Gate）的 ArchTest **真实 `.kcomp`** provider。
//!
//! 由生产路径 `create_component(name, args, IsolatedNative)` 创建（私有 AS +
//! 按域镜像 + Core 预置窗口），随后一个 KernelNative caller 经
//! `kcore_endpoint_call` 调用它的 `kcomp_service_dispatch`。
//!
//! # 零依赖、零 import
//!
//! Isolated 的 import 包络仍是**空集**（本增量不做 per-domain trampoline）：
//! 本夹具没有任何 UNDEF 符号，只用 `core::arch::asm!` 读 CSR 与自己镜像内的
//! load/store。**provider 不能调用 Core**——它只能读 Core 交付给它的邮箱与实例
//! 窗口。Core 侧的 endpoint publication（组件→Core 的 publish trampoline）也属于
//! 后续增量，因此 ArchTest 从 Core 侧登记本 provider 的 endpoint（见 selftest）。
//!
//! # 与 Core 的接口（increment 6 的窗口 + 邮箱协议）
//!
//! create（与 increment 5 同一窗口协议）：
//!
//! ```text
//! args      → 实例窗口基址 + 0（KcompCreateArgs）
//! out_state → 实例窗口基址 + 32（usize 槽；本组件写上报区地址，Core 读回）
//! tp        = 实例窗口基址 + 64（Core 安装的 per-instance runtime slot）
//! ```
//!
//! 本组件把 `*out_state` 指向**上报区**（`args + REPORT_OFF`）；Core 把它记成
//! 实例的 opaque state，并在每次 service dispatch 时作为第一个参数交回。上报区
//! 与邮箱（Core 拥有的另一个页）都在本实例的私有 AS 里，Core 从自己的 backing
//! 视图读回并断言。
//!
//! service dispatch（Core 经 gateway 交付 `a0..a3`）：
//!
//! ```text
//! a0 = instance_state（= 上报区地址，本组件自己写的）
//! a1 = port（Core 从 endpoint 记录解析）
//! a2 = method（caller 的标量参数）
//! a3 = frame（**实例域内**的 KcompCallFrame 描述符；args/input/output 也都是
//!      实例域内的邮箱 VA——caller 的缓冲在另一个 AS 里，本组件看不见）
//! ```
//!
//! dispatcher 把观察值写进上报区，ArchTest 从 Core 视图读回：它证明扁平帧真的
//! 被**拷贝**过边界（provider 看到的三个指针都在邮箱页内、内容等于 caller 的
//! 负载）、`port` / `method` / `state` / `tp` / `satp` 正确、并且它确实跑在私有
//! AS 里。
//!
//! # 故障注入
//!
//! `method == METHOD_FAULT` 时：把 args 的前 `usize` 字节当作目标地址
//! `read_volatile` ——ArchTest 传的是 **caller 域内**的地址，在私有 AS 里必然
//! 缺页（scause 13）。Core 的 gateway 故障分派把它收敛成 `Outcome::Faulted`
//! （provider 逻辑死亡 + 清理），caller 拿到类型化错误。若访问**没有** fault
//! （机制失效），dispatcher 记录读到的值并返回 `STATUS_FAULT_NOT_TAKEN`，让
//! ArchTest 显式失败。

#![no_std]

/// 与 Core 侧 `KcompCreateArgs` 逐字同形（`#[repr(C)]`，Core 视为不透明字节）。
#[repr(C)]
pub struct CreateArgs {
    config_abi: u64,
    config: *const u8,
    config_len: usize,
}

/// 与 Core 侧 `KcompCallFrame` 逐字同形（六个指针宽字段，32/64 位布局一致）。
#[repr(C)]
pub struct CallFrame {
    args: *const u8,
    args_len: usize,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: usize,
}

/// 上报区相对实例窗口基址的偏移（Core 不解释；ArchTest 按同一偏移读回）。
const REPORT_OFF: usize = 512;

/// 上报槽号（与 ArchTest 逐槽一致）。
const R_MAGIC: usize = 0;
const R_STATE: usize = 1;
const R_PORT: usize = 2;
const R_METHOD: usize = 3;
const R_FRAME: usize = 4;
const R_ARGS: usize = 5;
const R_ARGS_LEN: usize = 6;
const R_INPUT: usize = 7;
const R_INPUT_LEN: usize = 8;
const R_OUTPUT: usize = 9;
const R_OUTPUT_LEN: usize = 10;
const R_ARG0: usize = 11;
const R_ARG1: usize = 12;
const R_IN0: usize = 13;
const R_IN1: usize = 14;
const R_TP: usize = 15;
const R_SATP: usize = 16;
const R_CALLS: usize = 17;
const R_CREATE_MAGIC: usize = 18;
const R_DESTROY_MAGIC: usize = 19;
const R_FAULT_TARGET: usize = 20;
const R_FAULT_VALUE: usize = 21;

const REPORT_MAGIC: usize = 0x5356_4321; // "SVC!"
const CREATE_MAGIC: usize = 0x4352_4541; // "CREA"
const DESTROY_MAGIC: usize = 0x4445_5354; // "DEST"

/// Core 侧登记本 provider endpoint 时使用的 port（Core 不解释，只透传）。
pub const PORT_EXPECTED: u32 = 0x1001;
/// echo：`output[i] = input[i % input_len] ^ ECHO_XOR`（input 为空时填 ECHO_FILL）。
pub const METHOD_ECHO: u32 = 0x2001;
/// 故障注入：读 args[0..usize] 给出的地址（caller 域内，本 AS 不可达）。
pub const METHOD_FAULT: u32 = 0x2002;

const ECHO_XOR: u8 = 0x5A;
const ECHO_FILL: u8 = 0xA5;

const STATUS_OK: i32 = 0x5E;
const STATUS_BAD_PORT: i32 = -22; // -EINVAL
const STATUS_BAD_METHOD: i32 = -38; // -ENOSYS
const STATUS_FAULT_NOT_TAKEN: i32 = -5; // -EIO

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

/// 组件 ABI 的必需入口：`kcomp_instance_create(const KcompCreateArgs *, void **) -> i32`。
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn kcomp_instance_create(args: *const CreateArgs, out_state: *mut *mut ()) -> i32 {
    if args.is_null() || out_state.is_null() {
        return STATUS_BAD_PORT;
    }
    // SAFETY: Core 传入的 args 指向实例窗口内的 KcompCreateArgs（只读）；out_state
    // 指向窗口内的 usize 槽（可写）。两者都在本实例的私有 AS 里。
    unsafe {
        let report = (args as usize) + REPORT_OFF;
        let slots = report as *mut usize;
        slots.add(R_CREATE_MAGIC).write_volatile(CREATE_MAGIC);
        slots.add(R_TP).write_volatile(read_tp());
        slots.add(R_SATP).write_volatile(read_satp());
        slots.add(R_CALLS).write_volatile(0);
        out_state.write(report as *mut ());
    }
    0
}

/// 组件 ABI 的必需入口：`kcomp_instance_destroy(void *state) -> i32`。
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn kcomp_instance_destroy(state: *mut ()) -> i32 {
    if state.is_null() {
        return 0;
    }
    // SAFETY: state 是 create 写回的上报区地址（同一实例、仍然映射）。
    unsafe {
        (state as *mut usize)
            .add(R_DESTROY_MAGIC)
            .write_volatile(DESTROY_MAGIC);
    }
    0
}

/// 可选的 image 级服务入口：`kcomp_service_dispatch(state, port, method, frame)`。
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn kcomp_service_dispatch(
    state: *mut (),
    port: u32,
    method: u32,
    frame: *const CallFrame,
) -> i32 {
    if state.is_null() || frame.is_null() {
        return STATUS_BAD_PORT;
    }
    let slots = state as *mut usize;
    // SAFETY: frame 是 Core 写进本实例邮箱的描述符（本 AS 内可读）；三个负载区
    // 也都在邮箱页内。所有访问都在本实例的私有 AS 里。
    let frame = unsafe { &*frame };
    // SAFETY: 上报区在实例窗口内（Core 预置）；槽号与 ArchTest 一致。
    unsafe {
        let calls = slots.add(R_CALLS).read_volatile();
        slots.add(R_CALLS).write_volatile(calls + 1);
        slots.add(R_MAGIC).write_volatile(REPORT_MAGIC);
        slots.add(R_STATE).write_volatile(state as usize);
        slots.add(R_PORT).write_volatile(port as usize);
        slots.add(R_METHOD).write_volatile(method as usize);
        slots.add(R_FRAME).write_volatile(frame as *const CallFrame as usize);
        slots.add(R_ARGS).write_volatile(frame.args as usize);
        slots.add(R_ARGS_LEN).write_volatile(frame.args_len);
        slots.add(R_INPUT).write_volatile(frame.input as usize);
        slots.add(R_INPUT_LEN).write_volatile(frame.input_len);
        slots.add(R_OUTPUT).write_volatile(frame.output as usize);
        slots.add(R_OUTPUT_LEN).write_volatile(frame.output_len);
        slots.add(R_TP).write_volatile(read_tp());
        slots.add(R_SATP).write_volatile(read_satp());
        slots.add(R_ARG0).write_volatile(if frame.args_len > 0 {
            frame.args.read_volatile() as usize
        } else {
            0
        });
        slots.add(R_ARG1).write_volatile(if frame.args_len > 1 {
            frame.args.add(1).read_volatile() as usize
        } else {
            0
        });
        slots.add(R_IN0).write_volatile(if frame.input_len > 0 {
            frame.input.read_volatile() as usize
        } else {
            0
        });
        slots.add(R_IN1).write_volatile(if frame.input_len > 1 {
            frame.input.add(1).read_volatile() as usize
        } else {
            0
        });
    }
    if port != PORT_EXPECTED {
        return STATUS_BAD_PORT;
    }
    match method {
        METHOD_ECHO => {
            // SAFETY: output 区在邮箱页内、长度 = caller 声明的 output_len
            // （Core 已按容量拒绝超长帧）。
            unsafe {
                for index in 0..frame.output_len {
                    let byte = if frame.input_len == 0 {
                        ECHO_FILL
                    } else {
                        frame.input.add(index % frame.input_len).read_volatile() ^ ECHO_XOR
                    };
                    frame.output.add(index).write_volatile(byte);
                }
            }
            STATUS_OK
        }
        METHOD_FAULT => {
            // SAFETY: 故意访问 caller 域内的地址——在私有 AS 里必须缺页。
            let target = unsafe { (frame.args as *const usize).read_volatile() };
            // SAFETY: 上报槽（实例窗口内）。
            unsafe {
                slots.add(R_FAULT_TARGET).write_volatile(target);
            }
            // SAFETY: 故意 trap；本指令之后不会再被执行（组件被 Core 放弃）。
            let value = unsafe { core::ptr::read_volatile(target as *const usize) };
            // 访问没有 fault（机制失效）：记录读到的值，让 ArchTest 显式失败。
            // SAFETY: 上报槽（实例窗口内）。
            unsafe {
                slots.add(R_FAULT_VALUE).write_volatile(value);
            }
            STATUS_FAULT_NOT_TAKEN
        }
        _ => STATUS_BAD_METHOD,
    }
}

/// 精确契约指纹（手工锚定，与 `abi/component.toml` 的 `KCOMP_ABI` 同值）。
#[unsafe(no_mangle)]
pub static kcomp_abi: u64 = 0x4B43_4F4D_5041_4249;

/// 组件私有 panic handler：本夹具没有 panic 源，存在只为满足链接前提，且
/// **刻意不引 `kcore_*`**（空 import 包络）。panic = 自旋（Isolated 组件没有
/// panic-escape import 面；故障 containment 走 gateway 的 trap 路径）。
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
