//! Architecture backend crate: RISC-V ISA family plus host fake backend.
//!
//! 组织方式参照 Linux `arch/<isa>/` 与 seL4 `src/arch/{arm,riscv,x86}`：
//! 每个 ISA 一个独立单元，内部只放指令集相关代码：
//! trap 入口与分发、寄存器上下文与 context switch、MMU/TLB、
//! 用户态模式切换、中断开关、原子/CPU 原语。
//!
//! 启动入口 `_start` 接收 a0=hartid、a1=dtb 指针；新架构各自独立 crate。

#![no_std]
extern crate alloc;

#[cfg(all(feature = "supervisor", feature = "machine"))]
compile_error!("arch features `supervisor` and `machine` are mutually exclusive");

#[cfg(not(any(feature = "supervisor", feature = "machine")))]
compile_error!("arch requires exactly one privilege-mode feature: `supervisor` or `machine`");

#[cfg(all(feature = "vm-mmu", feature = "vm-nommu"))]
compile_error!("arch VM features `vm-mmu` and `vm-nommu` are mutually exclusive");

#[cfg(not(any(feature = "vm-mmu", feature = "vm-nommu")))]
compile_error!("arch requires exactly one VM feature: `vm-mmu` or `vm-nommu`");

#[cfg(test)]
extern crate std;

// Kconfig → Rust 的窄运输契约（`MAX_CPUS` 解析 / 校验）只在 host test 下编译进
// lib：测试锁定的是 build.rs 实际用的那一份实现（见 `src/build_config.rs`）。
#[cfg(test)]
mod build_config;

// 编译期 per-CPU 容量：由 `arch/build.rs` 从 Kconfig `MAX_CPUS` 生成。
// 这里是它的**唯一定义**；`core::machine::MAX_CPUS` 只是 re-export。
include!(concat!(env!("OUT_DIR"), "/max_cpus.rs"));

pub mod component;
/// 架构中立的 CPU 身份类型（`HardwareCpuId`）与回调形状。
pub mod cpu;
#[cfg(feature = "vm-nommu")]
pub mod nommu;
/// SMP 的 arch 契约（CPU 启动 + IPI 传输机制）。见 `smp.rs` 顶部边界说明。
pub mod smp;
pub mod store;
pub mod vm;

/// 当前构建 profile 的地址空间 backend。
///
/// VM profile 先决定 MMU / NoMMU；只有 MMU profile 才继续由 RISC-V XLEN
/// 选择 Sv39 或 Sv32。host 上的 MMU 页表算法测试不需要这个运行时 alias，
/// 因为真实的 `AddressSpace` wrapper 只在 RISC-V target 提供。
#[cfg(feature = "vm-nommu")]
pub type AddressSpaceImpl = nommu::NoMmuAddressSpace;

#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
pub type AddressSpaceImpl = riscv::mmu::address_space::Sv39AddressSpace;

#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
pub type AddressSpaceImpl = riscv::mmu::address_space::Sv32AddressSpace;

/// 新 ISA 的地址空间 backend 尚未实现：骨架用**显式占位类型**（`todo!()`），
/// 不假装成 Sv39，也不退化成 NoMMU。新 ISA bring-up 时原地替换成本 ISA 的页表后端。
#[cfg(all(
    feature = "vm-mmu",
    any(
        all(target_arch = "x86_64", target_os = "none"),
        all(target_arch = "aarch64", target_os = "none"),
        all(target_arch = "loongarch64", target_os = "none")
    )
))]
pub type AddressSpaceImpl = stub_vm::StubAddressSpace;

#[cfg(all(
    not(any(target_arch = "riscv32", target_arch = "riscv64")),
    not(target_os = "none")
))]
pub mod fake;

/// 未支持的裸机目标必须显式报错，**不能**静默落到 `Fake`（host 后端）。
#[cfg(all(
    target_os = "none",
    not(any(
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "loongarch64"
    ))
))]
compile_error!("unsupported bare-metal target: add an arch backend or build for host");

/// 新 ISA 的显式占位地址空间（仅骨架；见 [`AddressSpaceImpl`]）。
#[cfg(feature = "vm-mmu")]
pub mod stub_vm;

#[cfg(any(test, all(target_arch = "aarch64", target_os = "none")))]
pub mod aarch64;
#[cfg(any(test, all(target_arch = "loongarch64", target_os = "none")))]
pub mod loongarch64;
/// 新增 ISA 后端骨架：host `test` 下只编译纯编码模块（供单测）；
/// 裸机目标下编译完整后端（`cpu` / `smp` 等子模块按 `target_os = "none"` 门控）。
#[cfg(any(test, all(target_arch = "x86_64", target_os = "none")))]
pub mod x86_64;

// riscv 模块在所有目标都编译，但内部子模块按依赖门控：
// - ISA/asm 子模块（cpu/context/trap/firmware/console）只在真实 RISC-V 目标；
// - 纯算法子模块（elf 重定位、mmu 页表编码）host 也可编译 —— host 测试直接测
//   生产实现，而不是一份复制算法（见 docs/development/testing.md）。
// boot 期的内核页表策略（identity + high-half 双映射、段权限、临时 root）在
// boot crate `vm/`，不属于 arch。
pub mod riscv;

pub use store::{ComponentStore, StoreEntry, StoreError};

/// Component object ABI：按 ISA 选择重定位后端。
///
/// **不是**“纯编码逻辑所以哪都能用”：relocation 是组件的运行时对象 ABI，
/// 非 RISC-V 裸机目标必须用本 ISA 的后端（`ELF_MACHINE` 各不同）。新 ISA 后端
/// 在实现前对 `apply` 返回 `RelocationError::Unsupported`，绝不误用 RISC-V 语义。
///
/// host（`test` / 开发机）沿用 RISC-V 后端，因为现有 host fixture 与
/// `kcomp_abi_drift` 锁的是 RISC-V 组件对象；这不是“host 原生执行”。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type ComponentRelocationImpl = riscv::elf::RiscvRelocator;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub type ComponentRelocationImpl = x86_64::elf::X86_64Relocator;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub type ComponentRelocationImpl = aarch64::elf::Aarch64Relocator;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub type ComponentRelocationImpl = loongarch64::elf::Loongarch64Relocator;

#[cfg(all(
    not(any(target_arch = "riscv32", target_arch = "riscv64")),
    not(target_os = "none")
))]
pub type ComponentRelocationImpl = riscv::elf::RiscvRelocator;

/// Normalize a linked high-half address to its early identity/physical view.
///
/// Early boot keeps the whole RAM window identity-mapped, so the low alias of
/// a high-half kernel symbol is reachable from a low-address component (an
/// auipc+jalr pair only covers ±2 GiB).  Host builds have no high-half split
/// and use the address as-is.
///
/// 这是 arch 的地址方案原语（ELf relocator `normalize_symbol_address` 与 boot
/// selftest 都要用）；boot 侧的映射策略在 boot crate `vm/`，那里的
/// `HIGH_HALF_OFFSET` 与本处**必须保持一致**（由 linker.ld 布局决定）。
#[cfg(target_arch = "riscv64")]
const HIGH_HALF_OFFSET: usize = 0xffff_ffc0_0000_0000;

#[cfg(target_arch = "riscv64")]
pub fn physical_address_of(address: usize) -> usize {
    if address >= HIGH_HALF_OFFSET {
        address.wrapping_sub(HIGH_HALF_OFFSET)
    } else {
        address
    }
}

#[cfg(target_arch = "riscv32")]
pub fn physical_address_of(address: usize) -> usize {
    address
}

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
pub fn physical_address_of(address: usize) -> usize {
    address
}

pub enum ResetType {
    Shutdown,
    ColdReboot,
    WarmReboot,
}

/// CPU/ISA 原语：上下文、寄存器切换、中断开关和架构初始化。
///
/// 这个 trait 不包含 console、reset 或设备操作；那些属于 firmware/platform
/// 服务，由同一个具体 backend 分别实现相应的 trait。
pub trait CpuArch {
    /// 寄存器上下文类型。
    type Context;
    /// 中断开关的保存状态（RISC-V：状态寄存器；host fake：模拟的启用位）。
    /// C5 骨架：irq-save 临界区的状态载体。
    type IrqFlags;
    fn context_switch(from: &mut Self::Context, to: &Self::Context);
    fn new_context(entry: usize, stack_top: usize) -> Self::Context;
    /// 初始化**当前执行 CPU** 的架构执行/trap 状态。
    ///
    /// 不是一次性整机操作：boot 在 high-half 切换后可能对同一 CPU 再调一次以
    /// 重定位 trap 状态；但**不得**打开中断使能（那是 [`Self::enable_irq`]）。
    fn init_cpu();
    /// 打开当前 CPU 的**全局**中断使能位（各本地中断源已初始化的最后一步）。
    fn enable_irq();

    /// 关中断并返回先前状态（irq-save 临界区进入）。
    /// Riscv 保存状态寄存器；host fake 模拟嵌套状态供测试检查。
    fn disable_irq() -> Self::IrqFlags;
    /// 恢复 `disable_irq` 返回的状态（irq-restore 退出）；嵌套 guard 必须恢复进入时状态。
    fn restore_irq(flags: Self::IrqFlags);
    /// 等待下一次中断（RISC-V `wfi`；host fake = no-op）。
    ///
    /// **调用者负责保证有中断会到来**（例如先 arm 一个 one-shot timer）——
    /// 没有使能的中断源时 `wfi` 可能永久睡眠。
    fn wait_for_interrupt();

    // ——— SMP：CPU-local 身份与基址（SMP 骨架，见 docs/modules/arch.md）———
    //
    // 契约要点：
    // - `current_cpu` 是**快照**，不是跨调度/迁移保留本地引用的许可；
    // - per-CPU 基址是 **Core 拥有的不透明本地存储**，与任务执行状态（RISC-V
    //   `tp`）严格分离：`tp` 只是被透明保存 / 恢复的架构寄存器，不是第二份
    //   per-CPU 基址；
    // - `install_per_cpu_base` 在**被绑定的 CPU 上、关中断**调用，且该 CPU 变
    //   online 后不得再绑定（CPU 不迁移）。

    /// 当前执行 CPU 的**逻辑**身份（`CpuId`）；CPU-local 绑定安装前返回 `None`。
    ///
    /// arch 只存 Core 赋的逻辑 id，不自行推导；UP 阶段由 arch 提供 `CpuId(0)`。
    fn current_cpu() -> Option<cpu::CpuId>;

    /// 当前执行 CPU 的 Core 本地存储基址（不透明；arch 不解释其内容）。
    ///
    /// 这不是任务执行状态（`tp`）；实现载体随 ISA：RV S-mode = `sscratch`
    /// 指向的 arch 私有入口记录，x86_64 = kernel GS base，AArch64 = `TPIDR_EL1`，
    /// LoongArch = 显式保留的 `CSR.KSAVE`。
    fn per_cpu_base() -> Option<core::ptr::NonNull<()>>;

    /// 把 `base` 安装进 arch 私有的入口状态，并绑定 **Core 赋予的逻辑身份**。
    ///
    /// # Safety
    /// - 必须在本 CPU 上、关中断调用；
    /// - arch 入口状态已就绪；
    /// - `base` 在 Core 入口可达的任何地址空间都保持有效且可寻址；
    /// - `cpu` 是本硬件 CPU 唯一的逻辑身份（由 Core 赋号，不是硬件 id）；
    /// - 该 CPU 变 online 后不得重绑定。
    ///
    /// **注意**：`context_switch` 按任务上下文保存 / 恢复 `tp`（普通架构执行
    /// 状态），但 **不得**从任务上下文恢复 per-CPU 基址。
    unsafe fn install_per_cpu_base(cpu: cpu::CpuId, base: core::ptr::NonNull<()>);
}

/// 时钟 / 单次定时器服务（firmware/platform 能力，不是 ISA 原语）。
///
/// 与 `Console`/`SystemReset` 同一模式：Core 只依赖本 trait 与 `TimerImpl`，
/// 不感知 SBI/CLINT 细节。时间单位 = 平台的 timebase tick。
/// C5 骨架：签名即契约，实现待手写。
pub trait Timer {
    /// 初始化**当前执行 CPU** 的 timer（disarmed、source-masked）。
    ///
    /// 不得打开全局中断使能；由 [`CpuArch::enable_irq`] 在最后统一开闸。
    fn init_cpu() -> Result<(), smp::InitError>;
    /// 当前时间（单调递增）。
    fn now() -> u64;
    /// 编程下一次时钟中断的**绝对** deadline（与 `now` 同一基准）。
    fn set_deadline(deadline: u64);
    /// 取消当前 deadline，直到下一次 `set_deadline` 不再产生 timer IRQ。
    fn cancel_deadline();
    /// 注册时钟中断回调（回执带**逻辑** `CpuId`）；是否使能由
    /// [`Self::enable_timer_interrupt`] 单独负责。
    fn register_timer_handler(handler: cpu::LocalInterruptHandler);
    fn enable_timer_interrupt();
}

/// 外部中断控制器（PLIC）机制（C6 骨架）。
///
/// 与 `Timer`/`Console`/`SystemReset` 同一模式：Core 只依赖本 trait 与
/// `InterruptImpl`，不感知 PLIC 寄存器布局。`docs/architecture/overview.md` §3 把中断
/// 控制器的长期定位写成「驱动（由 discovery 发现）」——当前机制放在 arch（同
/// CLINT/timer），Core 侧只依赖 trait，后端实现可替换。
pub trait InterruptController {
    /// 控制器全局配置类型（后端特有；由 boot 构造，含板级 context 映射）。
    type Config;
    /// claim 令牌：携带 complete 所需的完整信息；**非 `Copy` / 非 `Send`**，
    /// 约束在同一 CPU 上 claim/complete 配对。
    type Claim;

    /// 全局配置（boot 一次）。
    ///
    /// # Safety
    /// `config` 描述的 MMIO 映射与资源必须常驻且有效。
    unsafe fn configure(config: Self::Config) -> Result<(), smp::InitError>;

    /// 初始化**当前执行 CPU** 的控制器接口（保持 masked）。
    fn init_cpu() -> Result<(), smp::InitError>;

    /// 允许 / 屏蔽一条外部中断线（作用于该线的**固定路由**，不是调用者 CPU）。
    fn enable(line: u32);
    fn disable(line: u32);

    /// 取一条 pending 外部中断；无 pending → `None`。
    fn claim() -> Option<Self::Claim>;
    /// 从 claim 令牌取线号（Core 路由用；令牌本身留作 complete）。
    fn claim_line(claim: &Self::Claim) -> u32;
    /// 通知控制器该中断已处理。
    fn complete(claim: Self::Claim);

    /// 注册外部中断回调（trap 分发调用；回执带**逻辑** `CpuId`）；
    /// Core 在 `irq::init` 时接入。
    fn register_external_handler(handler: cpu::LocalInterruptHandler);

    /// 只解除**当前执行 CPU** 的外部中断投递机制（不动全局中断使能位）。
    fn enable_external_interrupt();
}

/// 早期 console 服务。它是 boot/firmware 传输能力，不是 CPU ISA 原语。
pub trait Console {
    fn write_byte(byte: u8);
    fn getc() -> Option<u8>;
}

/// 系统 reset 服务。具体实现通常来自 SBI、UEFI 或平台固件。
pub trait SystemReset {
    fn system_reset(reset_type: ResetType) -> !;
}

/// 当前编译目标的 CPU backend。
///
/// 选择规则（新增 ISA 时必须保持互斥）：
/// - RISC-V → `Riscv`；
/// - bare-metal x86_64 / aarch64 / loongarch64 → 对应后端（骨架，`todo!()`）；
/// - 其余（host 上的 `kernel` 依赖、未支持目标）→ `Fake`。
/// **host 上的 `x86_64` 必须落到 `Fake`**：靠 `target_os = "none"` 区分裸机与
/// Linux host（不能用 `target_arch` 单独判，否则 host 测试会进特权代码）。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type CpuImpl = riscv::Riscv;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub type CpuImpl = crate::x86_64::X86_64;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub type CpuImpl = crate::aarch64::Aarch64;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub type CpuImpl = crate::loongarch64::Loongarch64;

#[cfg(all(
    not(any(target_arch = "riscv32", target_arch = "riscv64")),
    not(target_os = "none")
))]
pub type CpuImpl = fake::Fake;

/// 当前编译目标的 console backend。
///
/// 现在与 CPU backend 共享具体类型；当同一 ISA 支持多个 platform 时，
/// 这里可以改成由 boot profile 选择，而不改变 Core 的 Console trait。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type ConsoleImpl = riscv::Riscv;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub type ConsoleImpl = crate::x86_64::X86_64;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub type ConsoleImpl = crate::aarch64::Aarch64;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub type ConsoleImpl = crate::loongarch64::Loongarch64;

#[cfg(all(
    not(any(target_arch = "riscv32", target_arch = "riscv64")),
    not(target_os = "none")
))]
pub type ConsoleImpl = fake::Fake;

/// 当前编译目标的 reset backend。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type ResetImpl = riscv::Riscv;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub type ResetImpl = crate::x86_64::X86_64;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub type ResetImpl = crate::aarch64::Aarch64;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub type ResetImpl = crate::loongarch64::Loongarch64;

#[cfg(all(
    not(any(target_arch = "riscv32", target_arch = "riscv64")),
    not(target_os = "none")
))]
pub type ResetImpl = fake::Fake;

/// 当前编译目标的时钟/定时器 backend（C5 骨架；与 Console/Reset 同模式）。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type TimerImpl = riscv::Riscv;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub type TimerImpl = crate::x86_64::X86_64;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub type TimerImpl = crate::aarch64::Aarch64;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub type TimerImpl = crate::loongarch64::Loongarch64;

#[cfg(all(
    not(any(target_arch = "riscv32", target_arch = "riscv64")),
    not(target_os = "none")
))]
pub type TimerImpl = fake::Fake;

/// 当前编译目标的中断控制器 backend（C6 骨架；与 Console/Timer 同模式）。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type InterruptImpl = riscv::Riscv;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub type InterruptImpl = crate::x86_64::X86_64;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub type InterruptImpl = crate::aarch64::Aarch64;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub type InterruptImpl = crate::loongarch64::Loongarch64;

#[cfg(all(
    not(any(target_arch = "riscv32", target_arch = "riscv64")),
    not(target_os = "none")
))]
pub type InterruptImpl = fake::Fake;

/// 当前编译目标的 SMP backend（CPU 启动 + IPI 传输；SMP 骨架）。
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
pub type SmpImpl = riscv::Riscv;

#[cfg(all(target_arch = "x86_64", target_os = "none"))]
pub type SmpImpl = crate::x86_64::X86_64;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
pub type SmpImpl = crate::aarch64::Aarch64;

#[cfg(all(target_arch = "loongarch64", target_os = "none"))]
pub type SmpImpl = crate::loongarch64::Loongarch64;

#[cfg(all(
    not(any(target_arch = "riscv32", target_arch = "riscv64")),
    not(target_os = "none")
))]
pub type SmpImpl = fake::Fake;

/// Core 使用的任务上下文类型；Core 不关心具体 ISA 的寄存器布局。
pub type ContextImpl = <CpuImpl as CpuArch>::Context;
