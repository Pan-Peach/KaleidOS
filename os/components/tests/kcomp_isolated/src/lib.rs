//! kcomp_isolated —— 按域放段 / 页级权限分离的 ArchTest **真实 `.kcomp`**。
//!
//! 它**不被任何组件生命周期路径加载**：ArchTest（`os/boot/riscv/src/selftest.rs`）
//! 把它的字节从内嵌 kpkg 读出，走 `component::isolated_load` 放进一个私有 AS，
//! 再经 跨 AS trampoline 进入 `kcomp_instance_create`。
//!
//! # 为什么零依赖、零 import
//!
//! Isolated 的 import 包络是**空集**（`docs/architecture/deployment.md` §6.1）：
//! 本夹具只做本镜像内的 load/store 与自调用，不引用 `kcore_*`、不引用 SDK
//! （panic adapter 会带 `kcore_*` import），因此镜像里没有任何 UNDEF 符号。
//! 这也让它成为"按域装载不依赖 KernelNative 重定位结果"的最小真实样本。
//!
//! # 段形状（ArchTest 断言的对象）
//!
//! - `.text.*`：`kcomp_instance_create` / `kcomp_instance_destroy`（R+X）；
//! - `.rodata.*`：`RODATA_CELL`（R，不可写、不可执行）；
//! - `.data.*`：`DATA_CELL`（已初始化，R+W）；
//! - `.bss.*`：`BSS_CELL`（零初始化，R+W）。
//!
//! # 控制页协议（ArchTest 以 R+W 映射进实例 AS）
//!
//! 槽位按 `usize` 宽度（RV32 / RV64 各自一致）：
//!
//! ```text
//! 0 magic（组件写）      1 观察到的 satp（组件写）   2 command（Core 写）
//! 3 status（组件写）     4 text VA（组件写）          5 data VA（组件写）
//! 6 rodata VA（组件写）  7 rodata 读回值（组件写）    8 data 读回值（组件写）
//! 9 bss 读回值（组件写） 10 target VA（Core 写，仅 CMD_LOAD_TARGET）
//! 11 期望的实例 satp（Core 写；环境门禁，见下）
//! ```
//!
//! **环境门禁**：`create` 第一件事是比较 `csrr satp` 与控制页里 ArchTest 预写的
//! 实例 satp。不符（例如被 `monitor load` 当成 KernelNative 组件加载，控制页
//! VA 在 Core AS 里是别的东西）→ 立刻返回 `-EPERM` 且**不写任何槽位**：本夹具
//! 只在「ArchTest 已 prepare 的私有 AS」里有意义，绝不在别的环境里触碰那个
//! 硬编码 VA（QEMU virt 的 0x3000_0000 是 PCIe ECAM）。
//!
//! command：`0` 正常上报并返回 `0`；`1` 写自己的 text 页（store page fault）；
//! `2` 取自己的 data 页当函数调用（instruction page fault）；`3` 读 Core 提供的
//! target VA（该地址在实例 AS 内未映射 → load page fault）；其它 → `-EINVAL`。
//!
//! 这三个 fault 由 ArchTest 注册的 Core 窄策略观察（cause / stval / 该页在
//! Core ledger 里的权限）后判为不可恢复——**页表强制是真的**，不是"没映射也
//! 会 fault"的巧合：text 页与 data 页都在实例 AS 里，只是权限不允许该访问。

#![no_std]

// 控制页 VA：由 ArchTest 以 R+W 映射进实例 AS（测试夹具 I/O，不是组件资源）。
const CTL_BASE: usize = 0x3000_0000;

// 控制页槽号（字节偏移 = 槽号 × size_of::<usize>()）。
const CTL_MAGIC: usize = 0;
const CTL_SATP: usize = 1;
const CTL_COMMAND: usize = 2;
const CTL_STATUS: usize = 3;
const CTL_TEXT_VA: usize = 4;
const CTL_DATA_VA: usize = 5;
const CTL_RODATA_VA: usize = 6;
const CTL_RODATA_VALUE: usize = 7;
const CTL_DATA_VALUE: usize = 8;
const CTL_BSS_VALUE: usize = 9;
const CTL_TARGET_VA: usize = 10;
const CTL_EXPECT_SATP: usize = 11;

const MAGIC: usize = 0x4953_4f4c; // "ISOL"
const STATUS_INVALID_COMMAND: i32 = -22; // -EINVAL
const STATUS_WRONG_ENVIRONMENT: i32 = -1; // -EPERM

const CMD_REPORT: usize = 0;
const CMD_STORE_TEXT: usize = 1;
const CMD_FETCH_DATA: usize = 2;
const CMD_LOAD_TARGET: usize = 3;

/// `.rodata` 哨兵（只读段内容的可读性证据）。
static RODATA_CELL: usize = 0x524f_4441; // "RODA"

/// `.data` 哨兵（已初始化可写段）。
static mut DATA_CELL: usize = 0x4441_5441; // "DATA"

/// `.bss` 哨兵（零初始化可写段）。
static mut BSS_CELL: usize = 0;

fn ctl_slot(index: usize) -> *mut usize {
    (CTL_BASE + index * core::mem::size_of::<usize>()) as *mut usize
}

unsafe fn ctl_write(index: usize, value: usize) {
    // SAFETY: 调用方保证控制页已映射且槽位在页内。
    unsafe { core::ptr::write_volatile(ctl_slot(index), value) };
}

unsafe fn ctl_read(index: usize) -> usize {
    // SAFETY: 同上。
    unsafe { core::ptr::read_volatile(ctl_slot(index) as *const usize) }
}

fn read_satp() -> usize {
    let satp: usize;
    // SAFETY: 只读 CSR，无内存 / 栈副作用。
    unsafe {
        core::arch::asm!(
            "csrr {satp}, satp",
            satp = out(reg) satp,
            options(nostack, preserves_flags),
        );
    }
    satp
}

/// 上报本镜像各段的 VA 与读回值（真实 load/store，不经过任何 import）。
fn report() {
    // SAFETY: 本函数只在实例 AS 内执行，控制页与本镜像各段都已映射。
    unsafe {
        ctl_write(CTL_MAGIC, MAGIC);
        ctl_write(CTL_SATP, read_satp());
        ctl_write(CTL_TEXT_VA, kcomp_instance_create as *const () as usize);
        ctl_write(CTL_DATA_VA, core::ptr::addr_of!(DATA_CELL) as usize);
        ctl_write(CTL_RODATA_VA, core::ptr::addr_of!(RODATA_CELL) as usize);

        let rodata = core::ptr::read_volatile(core::ptr::addr_of!(RODATA_CELL));
        ctl_write(CTL_RODATA_VALUE, rodata);

        core::ptr::write_volatile(core::ptr::addr_of_mut!(DATA_CELL), DATA_CELL ^ 0x5555);
        let data = core::ptr::read_volatile(core::ptr::addr_of!(DATA_CELL));
        ctl_write(CTL_DATA_VALUE, data);

        core::ptr::write_volatile(core::ptr::addr_of_mut!(BSS_CELL), 0x4242_5353);
        let bss = core::ptr::read_volatile(core::ptr::addr_of!(BSS_CELL));
        ctl_write(CTL_BSS_VALUE, bss);
    }
}

/// 按控制页命令执行；fault 命令由 ArchTest 的 Core 窄策略观察（本函数不返回）。
fn run_command(command: usize) -> i32 {
    match command {
        CMD_REPORT => 0,
        CMD_STORE_TEXT => {
            let text = kcomp_instance_create as *const () as *mut u8;
            // SAFETY: 故意写自己 R+X 的 text 页——必须触发 store page fault。
            unsafe { core::ptr::write_volatile(text, 0xaa) };
            0
        }
        CMD_FETCH_DATA => {
            let data = core::ptr::addr_of!(DATA_CELL) as usize;
            // SAFETY: 故意取自己 R+W 的 data 页当代码执行——必须触发
            // instruction page fault。
            let entry: extern "C" fn() = unsafe { core::mem::transmute(data) };
            entry();
            0
        }
        CMD_LOAD_TARGET => {
            // SAFETY: 故意读 Core 提供的 target VA（实例 AS 内未映射）——
            // 必须触发 load page fault。
            let value =
                unsafe { core::ptr::read_volatile(ctl_read(CTL_TARGET_VA) as *const usize) };
            // SAFETY: 若上面没有 fault（不应发生），把读到的值留在 status 上便于诊断。
            unsafe { ctl_write(CTL_STATUS, value) };
            STATUS_INVALID_COMMAND
        }
        _ => STATUS_INVALID_COMMAND,
    }
}

/// 组件 ABI 的必需入口。Core 视角的签名是
/// `kcomp_instance_create(const KcompCreateArgs *, void **) -> i32`；
/// 本夹具忽略参数（ArchTest 经跨 AS trampoline 进入，不搬运 create 参数）。
#[unsafe(no_mangle)]
pub extern "C" fn kcomp_instance_create(_args: *const (), _out_state: *mut *mut ()) -> i32 {
    // 环境门禁（见模块文档）：只在 ArchTest prepare 过的私有 AS 里工作；
    // 其它环境（如被 monitor 当 KernelNative 组件加载）立刻拒绝且不写任何槽位。
    let expected_satp = unsafe { ctl_read(CTL_EXPECT_SATP) };
    if expected_satp == 0 || expected_satp != read_satp() {
        return STATUS_WRONG_ENVIRONMENT;
    }
    report();
    let command = unsafe { ctl_read(CTL_COMMAND) };
    let status = run_command(command);
    unsafe { ctl_write(CTL_STATUS, status as usize) };
    status
}

#[unsafe(no_mangle)]
pub extern "C" fn kcomp_instance_destroy(_state: *mut ()) -> i32 {
    0
}

/// 精确契约指纹（手工锚定，与 `abi/component.toml` 的 `KCOMP_ABI` 同值：
/// 8 字节 ASCII `b"KCOMPABI"` 的大端读数；Core 在装载时逐位校验）。
#[unsafe(no_mangle)]
pub static kcomp_abi: u64 = 0x4B43_4F4D_5041_4249;

/// 组件私有 panic handler：Rust 要求 `no_std` staticlib 提供它。本夹具**没有
/// 任何 panic 源**（不调用 SDK、不做可失败运算），因此它不会被引用、GC 后也
/// 不进镜像；存在只为满足 rustc 的链接前提，且**刻意不引 `kcore_*`**（空 import
/// 包络）。
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
