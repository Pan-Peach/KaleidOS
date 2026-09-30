//! RISC-V SMP 的 **boot 侧**骨架：AP 入口 trampoline、AP 初始栈、启动描述符与调用点。
//!
//! # 边界（对齐 `AGENTS.md` 与 `docs/modules/arch.md`）
//!
//! boot 只负责**启动期的物理 / 地址空间交接**这一类 boot policy：
//!
//! - `_secondary_start`（`secondary64.S`）：SBI HSM 把 AP 送到的**物理**入口。
//!   此时 `satp=0`、`SIE=0`、`a0=hartid`、`a1=opaque`；它把 AP 从物理世界带进
//!   长期内核地址空间（`satp` → 高半区），然后跳进 [`secondary_main`]。
//! - [`ApBoot`]：每 AP 一份、由主 hart 填好的启动描述符（物理栈 / 内核栈 / `satp` /
//!   入口 / 参数）。**布局即 ABI**，与 `secondary64.S` 的偏移逐字绑定。
//! - [`start_secondaries`]：主 hart 在 `kernel::init` 与长期地址空间建立后调用，
//!   为每个非 boot CPU 发布描述符并请求 arch 启动 AP。
//!
//! **不在 boot**：逻辑 CPU 身份 / 启动状态 / pending work / 调度归属（Core，
//! `kernel::smp`），以及 SBI HSM / IPI 的传输（arch，`arch::riscv::smp`）。
//!
//! # 骨架状态
//!
//! 类型与入口签名已定；[`start_secondaries`] / [`secondary_main`] 与
//! `secondary64.S` 的步骤体为 `todo!()`，由人类手写。本模块只在
//! `riscv64 + vm-mmu + smp` 下编译，默认（单 CPU）构建路径不包含它，因此现有
//! host / rv64 / rv32 测试保持全绿。

use crate::vm::bootstrap;
use core::arch::global_asm;
use core::mem::MaybeUninit;
use core::sync::atomic::{fence, AtomicUsize, Ordering};
use kernel::machine::{MachineInfo, MAX_CPUS};

global_asm!(include_str!("secondary64.S"));

unsafe extern "C" {
    /// `secondary64.S` 里的 AP 物理入口。
    fn _secondary_start();
}

/// 已在**非 boot CPU** 上跑过 IPI handler 的次数（`smp-ipi` 的证据）。
#[allow(dead_code)] // 只被 selftest 的 `smp-ipi` 用例读
pub static IPI_SEEN: AtomicUsize = AtomicUsize::new(0);

/// 每个 AP 在进入 Rust 之后的初始内核栈大小。
const AP_STACK_BYTES: usize = 16 * 1024;

#[allow(dead_code)] // 只取它的地址（栈顶），字段本身不读
#[repr(align(16))]
struct ApStack([u8; AP_STACK_BYTES]);

/// AP 内核栈池（每个逻辑 CPU 一张）。放在 `.bss.ap_stacks`（被链接脚本的
/// `*(.bss .bss.*)` 收进镜像的 NOLOAD 段）。其**物理**地址由镜像加载地址 +
/// 段偏移推出（见 [`start_secondaries`]）。
#[used]
#[unsafe(link_section = ".bss.ap_stacks")]
static mut AP_STACKS: [ApStack; MAX_CPUS] = [const { ApStack([0; AP_STACK_BYTES]) }; MAX_CPUS];

/// 单个 AP 的启动描述符。由主 hart 填写，trampoline 按固定偏移读取。
///
/// **布局即 ABI**：字段顺序 / 偏移与 `secondary64.S` 逐字绑定（见下方断言）。
#[repr(C)]
pub struct ApBoot {
    /// 进入 Rust 之前的物理栈顶（`satp=0` 时可用）。
    pub boot_stack_top: usize,
    /// 切换长期地址空间后使用的内核栈顶（高半区 VA）。
    pub kernel_stack_top: usize,
    /// 长期内核根页表的 `satp` 值（`MODE | PPN`）。
    pub satp: usize,
    /// 高半区 Rust 入口（[`secondary_main`]）。
    pub entry: usize,
    /// 传给 Rust 入口的不透明值（约定为逻辑 CPU 的稠密下标，由 Core 校验）。
    pub argument: usize,
}

#[used]
#[unsafe(link_section = ".bss.ap_boot")]
static mut AP_BOOT: [MaybeUninit<ApBoot>; MAX_CPUS] = [const { MaybeUninit::uninit() }; MAX_CPUS];

fn ap_boot_phys(i: usize) -> usize {
    bootstrap::physical_address_of(unsafe { core::ptr::addr_of!(AP_BOOT[i]) as usize })
}

// 布局即 ABI：偏移钉死在 `secondary64.S` 的常量上，漂移即编译失败。
const _: () = {
    let w = core::mem::size_of::<usize>();
    assert!(core::mem::offset_of!(ApBoot, boot_stack_top) == 0);
    assert!(core::mem::offset_of!(ApBoot, kernel_stack_top) == w);
    assert!(core::mem::offset_of!(ApBoot, satp) == 2 * w);
    assert!(core::mem::offset_of!(ApBoot, entry) == 3 * w);
    assert!(core::mem::offset_of!(ApBoot, argument) == 4 * w);
};

fn stack_high_top(i: usize) -> usize {
    (unsafe { core::ptr::addr_of!(AP_STACKS[i]) as usize }) + AP_STACK_BYTES
}

fn stack_low_top(i: usize) -> usize {
    bootstrap::physical_address_of(stack_high_top(i))
}

/// `_secondary_start` 的**物理**地址：SBI `hart_start` 的入口。
///
/// 它在正式 `.text`（高 VMA、低 LMA）：物理入口 = VMA - `HIGH_HALF_OFFSET`，
/// 也就是它的装载 LMA；AP 从那里执行（satp=0）。
pub fn secondary_entry_address() -> usize {
    bootstrap::physical_address_of(_secondary_start as *const () as usize)
}

/// AP 的高半区 Rust 入口：由 `secondary64.S` 在长期地址空间生效后跳入。
///
/// boot 只保证「站在高半区、关中断、`tp = 0`、栈已就位」；其余
/// （入口记录绑定、本地子系统、就绪 / 门控 / Online、空闲循环）全部交给 Core 的
/// `smp::secondary_entry`。它**永不返回**。
pub extern "C" fn secondary_main(argument: usize) -> ! {
    // SAFETY: `secondary64.S` 已建立 `SecondaryEntry` 契约要求的入口环境。
    unsafe { kernel::smp::secondary_entry(argument) }
}

/// 主 hart 启动所有次 CPU。
///
/// 前置：`kernel::init` 已完成、长期内核地址空间（runtime root）已建立并激活。
/// boot 只负责物理启动；**Core 拥有启动真相**（`request_start` 置 `Starting`），
/// 并在 [`kernel::smp::release_secondaries`] 里等全部 AP Ready 后放行启动屏障。
pub fn start_secondaries(info: &MachineInfo) {
    let satp = arch::riscv::mmu::current_satp();
    let entry = secondary_main as *const () as usize;
    let trampoline = secondary_entry_address();

    for (i, cpu) in info.cpu_info.iter().enumerate() {
        if cpu.hardware_id == info.boot_hardware_id {
            continue;
        }
        // Core 记「已请求启动」（Offline → Starting）；boot 随后才做物理启动。
        kernel::smp::request_start(kernel::machine::CpuId::from_raw(i))
            .expect("SMP: request_start rejected");

        let ap = ApBoot {
            boot_stack_top: stack_low_top(i),
            kernel_stack_top: stack_high_top(i),
            satp,
            entry,
            argument: i,
        };
        unsafe { core::ptr::addr_of_mut!(AP_BOOT[i]).write(MaybeUninit::new(ap)) };
        fence(Ordering::Release);
        let result = arch::riscv::firmware::hart_start(
            cpu.hardware_id.raw() as usize,
            trampoline,
            ap_boot_phys(i),
        );
        if result.is_err() {
            panic!(
                "[SMP] CPU {}: hart_start({:#x}) failed: {:?}",
                i,
                cpu.hardware_id.raw(),
                result
            );
        }
    }

    // 等所有已请求的 AP Ready 后放行启动屏障（并打开 BSP 的 IPI 源）。fail-closed。
    kernel::smp::release_secondaries().expect("SMP: secondary bring-up failed");
}
