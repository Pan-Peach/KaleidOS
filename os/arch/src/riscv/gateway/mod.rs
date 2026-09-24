//! S-mode **assembly gateway**：Isolated 域私有 AS 的唯一切换出口。
//!
//! 设计（`docs/architecture/deployment.md` §6.3 / `memory-and-heap.md` §8）：
//! 切换**不是** Rust 的 `activate()` 三明治。Core 侧先在持锁状态下完成全部
//! 校验并取出 `PreparedActivation`（`Copy`，无引用），然后只有本模块的双映射
//! 汇编在跑：它只做 `satp` + 栈/上下文切换，不碰 Rust、不取锁、不碰 Core 栈。
//! 目标 root 生效后到 Core root 恢复前，只允许触碰**双映射**的 gateway 代码页
//! / scratch 页与组件自己的栈。
//!
//! ```text
//! Core Rust（持锁准备、Release 锁）
//!    │  gateway::enter(Transition)          ← 唯一入口；关中断后进入
//!    ▼
//! gateway_enter（汇编，双映射页）
//!    │  保存 Core 现场 → stvec=gateway_trap_entry, sscratch=&scratch
//!    │  csrw satp=目标 root; sfence.vma → sp=组件栈, tp=slot, ra=返回点, jalr entry
//!    ▼
//! 组件在私有 AS 上运行（S-mode；可被 trap）
//!    │  正常返回 ─────────────────────────► 恢复 Core root / 现场 → ret
//!    └─ trap ──► gateway_trap_entry（汇编）→ 保存完整现场、切 Core root + 专用
//!                 trap 栈 → Rust 分派（timer/external 走普通主处理；异常交给 Core
//!                 钩子）→ Resume（切回实例 root，按 trap 帧 sret）/ Abandon
//!                 （恢复挂起的 Core 调用者，Outcome::Faulted）
//! ```
//!
//! # 双映射（dual-mapped gateway）
//!
//! [`code_page`] / [`scratch_page`] 各自独占一个物理页，在 Core AS 与实例 AS
//! 中**同 VA → 同 PA**。因此 `csrw satp` 前后 PC 连续、scratch 可读写。两页由
//! Core 侧的 `KernelAddressSpace` 落成实例侧映射（`component::isolated::prepare`）；
//! 实例 AS 里不会因此出现任何普通 Core 段 / Core 堆 / 页表 / MMIO。
//!
//! # 非显然硬件约束（诚实声明）
//!
//! - **协作式、非对抗边界**：S-mode 组件与 Core 同特权级，可以直接改 `satp` /
//!   `stvec` / 自己的 trampoline；这里证明的是**机制**（真页表、真 trap 往返），
//!   不是对抗隔离。真正的强制边界是 U-mode（SandboxedNative，未实现）。
//! - **ASID 恒 0 + 全量 `sfence.vma`**：不实现 ASID 分配 / 复用，也不声称支持。
//! - **`gp` 不是可互换环境**：本镜像没有 `__global_pointer$`、Rust 代码不依赖
//!   `gp`；gateway 仍把它当现场的一部分保存 / 恢复，并在调用 Core Rust 分派前
//!   装回 Core 的 `gp`。组件侧**不**安装自己的 `gp`（按域 gp 环境不在 arch）。
//! - **`tp` = 组件 runtime slot**：进入前由 [`Transition::runtime_slot`] 提供
//!   （0 = 无 slot）；只有 `tp` 的搬运，没有 TLS / 记账。
//! - **入口参数 `a0` .. `a3`**：由 [`Transition::arg0`] .. [`Transition::arg3`]
//!   写入 scratch，进入前装入（入口 ABI 的解释权在 Core；arch 不解释）。
//!   create / destroy 只用 `a0` / `a1`；service dispatch 用满四个（state / port /
//!   method / frame）。
//! - **单 CPU、不可重入**：scratch 与 Core trap 栈是单一静态；组件生命周期
//!   （`component/isolated_lifecycle.rs` 是生产调用方）同步进入、不嵌套，
//!   ArchTest 也串行驱动。
//!
//! # 故障归属
//!
//! `gateway_trap_entry` 只把 **phase == COMPONENT** 的 trap 交给 Core 分派钩子；
//! 过渡窗口（进入 / 退出未完成）或普通 Core 上下文里的 trap 一律 **fatal**
//! （[`gateway_fatal`]）。组件身份本身**不**构成"可恢复"的证明：arch 只做
//! eligibility（来源是 gateway + phase），是否恢复由 Core 注册的
//! [`ComponentFaultHandler`] 决定。

use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::mmu::SatpActivation;
use super::trap::{Interrupt, Scause, Trap, TrapFrame};
use crate::vm::{DualMappedPage, MappingPermission, PhysicalRange, VirtualRange};
use crate::{CpuArch, CpuImpl};

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("gateway64.S"));

#[cfg(target_arch = "riscv32")]
global_asm!(include_str!("gateway32.S"));

/// 一次同步切换的输入（Core 侧组装；**全部是 `Copy` 值**）。
///
/// arch 只消费、不解释：`activation` 是 backend 预打包的 satp，`entry` /
/// `stack_top` 是目标 AS 内已由 Core 验证过的 VA。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub activation: SatpActivation,
    /// 组件入口 VA（目标 AS 内可执行）。
    pub entry: usize,
    /// 组件栈顶 VA（目标 AS 内可读写，16 字节对齐）。
    pub stack_top: usize,
    /// 组件运行时的 runtime slot（写入 `tp`；0 = 无 slot）。
    pub runtime_slot: usize,
    /// 组件初始是否开中断（`sstatus.SIE`）。
    pub interrupts_enabled: bool,
    /// 故障归因 token（Core 真相：AS owner 的 raw id；arch 只透传）。
    pub fault_token: usize,
    /// 组件入口 `a0`（Core 已验证的值，arch 只搬运；入口 ABI 的解释权在 Core）。
    pub arg0: usize,
    /// 组件入口 `a1`（同 [`Transition::arg0`]）。
    pub arg1: usize,
    /// 组件入口 `a2`（同 [`Transition::arg0`]；create / destroy 为 0）。
    pub arg2: usize,
    /// 组件入口 `a3`（同 [`Transition::arg0`]；create / destroy 为 0）。
    pub arg3: usize,
}

/// 同步切换的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 组件正常返回；`usize` = 组件返回值（`a0`）。
    Returned(usize),
    /// 组件上下文里的异常被 Core 判为不可恢复：组件被放弃，控制权回到进入前
    /// 的 Core 调用者。被打断的组件现场保留在 scratch 中（仅诊断）。
    Faulted,
}

/// 组件上下文故障的分派决定（由 Core 注册的钩子返回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum FaultDecision {
    /// 不可恢复：把控制权交回 Core（[`Outcome::Faulted`]）。
    Abandon = 0,
    /// 可恢复：按（可能被钩子修改过的）trap 帧恢复组件并 `sret`。
    Resume = 1,
}

/// Core 注册的**窄故障分派钩子**。arch 只保证 provenance（trap 来自 gateway
/// 入口且 `phase == COMPONENT`）并把现场交给它；是否可恢复由 Core 决定。
pub type ComponentFaultHandler =
    fn(frame: *mut TrapFrame, scause: usize, stval: usize, token: usize) -> FaultDecision;

static FAULT_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// 注册组件故障分派钩子（后注册覆盖先注册；Core 只注册一次）。
pub fn register_component_fault_handler(handler: ComponentFaultHandler) {
    FAULT_HANDLER.store(handler as usize, Ordering::Release);
}

const PAGE_SIZE: usize = 4096;
const STACK_ALIGNMENT: usize = 16;
const CORE_TRAP_STACK_BYTES: usize = 32 * 1024;

// 相位值是与 `gateway64.S` / `gateway32.S` 的 `.equ PHASE_*` 共享的 ABI；
// Rust 只直接引用 `PHASE_IDLE`（初始值），其余由汇编消费。
#[allow(dead_code)]
const PHASE_IDLE: usize = 0;
#[allow(dead_code)]
const PHASE_TO_COMPONENT: usize = 1;
#[allow(dead_code)]
const PHASE_COMPONENT: usize = 2;
#[allow(dead_code)]
const PHASE_TO_CORE: usize = 3;

/// 同步路径上的 Core 恢复现场（进入时保存、返回 / abandon 时恢复）。
#[repr(C)]
struct CoreResume {
    ra: usize,
    sp: usize,
    gp: usize,
    tp: usize,
    s: [usize; 12],
    sstatus: usize,
    satp: usize,
    stvec: usize,
    sscratch: usize,
    trap_stack_top: usize,
}

/// gateway 的单页 scratch：被打断的完整现场 + 挂起 Core 的恢复现场 + 控制字。
///
/// 布局即 ABI：字段偏移与 `gateway64.S` / `gateway32.S` 的 `.equ` 常量**逐字
/// 一致**（见下方 `const _` 断言）。整页精确 4096 字节、4096 对齐，保证它只
/// 与自己的页重合——映射进实例 AS 时不会带上任何 Core 数据。
#[repr(C, align(4096))]
struct GatewayScratch {
    /// 组件 / 被打断执行者的完整寄存器现场（与 `TrapFrame` 同形）。
    frame: TrapFrame,
    /// 挂起的 Core 现场。
    core: CoreResume,
    /// 目标实例 root 的预打包 satp（trap 恢复时切回组件用）。
    instance_satp: usize,
    /// 组件运行期的 `stvec`（gateway trap 入口）。
    entry_stvec: usize,
    /// 相位：`PHASE_*`（fatal 判定）。
    phase: usize,
    /// 结果状态：0 = `Returned`，1 = `Faulted`。
    status: usize,
    /// 故障归因 token（Core 透传）。
    token: usize,
    /// 组件入口 `a0`（进入前装入；与 `.S` 的 `G_ENTRY_ARG0` 一致）。
    entry_arg0: usize,
    /// 组件入口 `a1`（与 `.S` 的 `G_ENTRY_ARG1` 一致）。
    entry_arg1: usize,
    /// 组件入口 `a2`（与 `.S` 的 `G_ENTRY_ARG2` 一致）。
    entry_arg2: usize,
    /// 组件入口 `a3`（与 `.S` 的 `G_ENTRY_ARG3` 一致）。
    entry_arg3: usize,
    _pad: [u8; PAGE_SIZE - SCRATCH_HEADER_BYTES],
}

/// 头部（除 `_pad`）字节数（两条路径都不含填充）。
const SCRATCH_HEADER_BYTES: usize = core::mem::size_of::<TrapFrame>()
    + core::mem::size_of::<CoreResume>()
    + 9 * core::mem::size_of::<usize>();

// 偏移即 ABI：与 `gateway64.S` / `gateway32.S` 顶部 `.equ` 常量钉死一致。
const _: () = {
    assert!(core::mem::offset_of!(GatewayScratch, frame) == 0);
    assert!(core::mem::offset_of!(GatewayScratch, core) == core::mem::size_of::<TrapFrame>());
    assert!(core::mem::offset_of!(CoreResume, ra) == 0);
    assert!(core::mem::offset_of!(CoreResume, sp) == core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(CoreResume, gp) == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(CoreResume, tp) == 3 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(CoreResume, s) == 4 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(CoreResume, sstatus) == 16 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(CoreResume, satp) == 17 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(CoreResume, stvec) == 18 * core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(CoreResume, sscratch) == 19 * core::mem::size_of::<usize>());
    assert!(
        core::mem::offset_of!(CoreResume, trap_stack_top) == 20 * core::mem::size_of::<usize>()
    );
    // 入口参数：偏移与 `.S` 的 `G_ENTRY_ARG0` .. `G_ENTRY_ARG3` 一致。
    #[cfg(target_arch = "riscv64")]
    {
        assert!(core::mem::offset_of!(GatewayScratch, entry_arg0) == 480);
        assert!(core::mem::offset_of!(GatewayScratch, entry_arg1) == 488);
        assert!(core::mem::offset_of!(GatewayScratch, entry_arg2) == 496);
        assert!(core::mem::offset_of!(GatewayScratch, entry_arg3) == 504);
    }
    #[cfg(target_arch = "riscv32")]
    {
        assert!(core::mem::offset_of!(GatewayScratch, entry_arg0) == 240);
        assert!(core::mem::offset_of!(GatewayScratch, entry_arg1) == 244);
        assert!(core::mem::offset_of!(GatewayScratch, entry_arg2) == 248);
        assert!(core::mem::offset_of!(GatewayScratch, entry_arg3) == 252);
    }
    assert!(core::mem::size_of::<GatewayScratch>() == PAGE_SIZE);
};

impl GatewayScratch {
    const ZERO: Self = Self {
        frame: TrapFrame {
            x: [0; 32],
            status: 0,
            epc: 0,
        },
        core: CoreResume {
            ra: 0,
            sp: 0,
            gp: 0,
            tp: 0,
            s: [0; 12],
            sstatus: 0,
            satp: 0,
            stvec: 0,
            sscratch: 0,
            trap_stack_top: 0,
        },
        instance_satp: 0,
        entry_stvec: 0,
        phase: PHASE_IDLE,
        status: 0,
        token: 0,
        entry_arg0: 0,
        entry_arg1: 0,
        entry_arg2: 0,
        entry_arg3: 0,
        _pad: [0; PAGE_SIZE - SCRATCH_HEADER_BYTES],
    };
}

#[repr(align(16))]
struct CoreTrapStack([u8; CORE_TRAP_STACK_BYTES]);

/// gateway scratch：单 CPU、不可重入（见模块文档的边界声明）。
static mut SCRATCH: GatewayScratch = GatewayScratch::ZERO;

/// 专用 Core trap 栈：只在 Core root + Core AS 下使用（**不**映射进实例 AS）。
static mut CORE_TRAP_STACK: CoreTrapStack = CoreTrapStack([0; CORE_TRAP_STACK_BYTES]);

unsafe extern "C" {
    fn gateway_enter(
        scratch: *mut GatewayScratch,
        satp: usize,
        entry: usize,
        stack_top: usize,
        slot: usize,
        irq_enable: usize,
        token: usize,
    );
    fn gateway_trap_entry();
    static gateway_code_start: u8;
    static gateway_code_end: u8;
}

/// gateway 的机制页：`[代码页(RX), scratch 页(RW)]`，都由 Core 在 `prepare`
/// 阶段按**同 VA → 同 PA** 落成实例侧映射。
pub fn pages() -> [DualMappedPage; 2] {
    [code_page(), scratch_page()]
}

/// 双映射的 gateway **代码页**（RX；实例 AS 里映射同一 VA → 同一 PA）。
///
/// `.S` 用 `.balign 4096 + .space 4096` 保证该页整页独占（代码之后的填充使下
/// 一个 input section 不可能落进同一页）；这里断言代码确实放得下一页。
pub fn code_page() -> DualMappedPage {
    let va = core::ptr::addr_of!(gateway_code_start) as usize;
    let end = core::ptr::addr_of!(gateway_code_end) as usize;
    let size = end
        .checked_sub(va)
        .expect("gateway code symbols must be ordered");
    assert!(
        va.is_multiple_of(PAGE_SIZE),
        "gateway code page must be page aligned (got {va:#x})"
    );
    assert!(
        size <= PAGE_SIZE,
        "gateway code must fit in one page (got {size})"
    );
    DualMappedPage {
        virtual_range: VirtualRange {
            base: va,
            size: PAGE_SIZE,
        },
        physical_range: PhysicalRange {
            base: crate::physical_address_of(va),
            size: PAGE_SIZE,
        },
        permission: MappingPermission::READ | MappingPermission::EXECUTE,
    }
}

/// 双映射的 gateway **scratch 页**（RW；实例 AS 里映射同一 VA → 同一 PA）。
pub fn scratch_page() -> DualMappedPage {
    let va = core::ptr::addr_of!(SCRATCH) as usize;
    assert!(
        va.is_multiple_of(PAGE_SIZE),
        "gateway scratch must be page aligned (got {va:#x})"
    );
    DualMappedPage {
        virtual_range: VirtualRange {
            base: va,
            size: PAGE_SIZE,
        },
        physical_range: PhysicalRange {
            base: crate::physical_address_of(va),
            size: PAGE_SIZE,
        },
        permission: MappingPermission::READ | MappingPermission::WRITE,
    }
}

/// Core 专用 trap 栈的半开区间 `[base, top)`（ArchTest / 诊断断言用）。
pub fn core_trap_stack_range() -> (usize, usize) {
    // SAFETY: 只取静态数组的地址（不读取内容、不创建引用）。
    let base = unsafe { core::ptr::addr_of!(CORE_TRAP_STACK.0) } as usize;
    (base, base + CORE_TRAP_STACK_BYTES)
}

fn core_trap_stack_top() -> usize {
    core_trap_stack_range().1 & !(STACK_ALIGNMENT - 1)
}

/// 进入目标私有 AS 执行 `transition.entry`，返回后再回到本调用者。
///
/// 必须在**无 Core 锁**的状态下调用（本函数不取任何锁）。中断在这里全程被
/// 屏蔽：进入窗口关中断，目标执行按 `interrupts_enabled` 开闸，返回时恢复调用
/// 者的中断状态。
pub fn enter(transition: Transition) -> Outcome {
    assert!(
        transition.entry != 0 && transition.stack_top.is_multiple_of(STACK_ALIGNMENT),
        "gateway transition is not prepared: entry={:#x} stack_top={:#x}",
        transition.entry,
        transition.stack_top
    );

    // SAFETY: [Category 2 — Data races] single-CPU, non-reentrant: the
    // component lifecycle enters synchronously and never nests a
    // transition; ArchTest also drives it serially.
    let scratch = core::ptr::addr_of_mut!(SCRATCH);
    unsafe {
        (*scratch).entry_stvec = gateway_trap_entry as *const () as usize;
        (*scratch).core.stvec = super::trap::vector_address();
        (*scratch).core.trap_stack_top = core_trap_stack_top();
        (*scratch).status = 0;
        (*scratch).entry_arg0 = transition.arg0;
        (*scratch).entry_arg1 = transition.arg1;
        (*scratch).entry_arg2 = transition.arg2;
        (*scratch).entry_arg3 = transition.arg3;
    }

    let flags = <CpuImpl as CpuArch>::disable_irq();
    // SAFETY: 描述符来自 Core 的 `prepare`（实例 AS 已映射 gateway 两页 + 入口 /
    // 栈）；scratch 是单页静态；汇编只写 scratch / Core 现场并在返回前恢复。
    unsafe {
        gateway_enter(
            scratch,
            transition.activation.satp(),
            transition.entry,
            transition.stack_top,
            transition.runtime_slot,
            usize::from(transition.interrupts_enabled),
            transition.fault_token,
        );
    }

    // 先观察结果、再恢复中断：返回与观察之间不留可被 trap 打断的窗口。
    // SAFETY: the assembly stored the outcome here before `ret`.
    let outcome = unsafe {
        match (*scratch).status {
            0 => Outcome::Returned((*scratch).frame.x[10]),
            _ => Outcome::Faulted,
        }
    };
    <CpuImpl as CpuArch>::restore_irq(flags);
    outcome
}

/// gateway trap 入口的 Rust 分派（汇编调用；Core root + 专用 trap 栈）。
///
/// - `Timer` / `External` 中断：走**普通 Core 主处理**（与 `trap_handler` 相同），
///   返回 `Resume`；
/// - 异常：交给注册的 [`ComponentFaultHandler`]；未注册 = 不可恢复 = fatal；
/// - 其它中断：与普通路径一致，fatal。
#[unsafe(no_mangle)]
extern "C" fn gateway_trap_dispatch(
    frame: *mut TrapFrame,
    scause: usize,
    stval: usize,
    token: usize,
) -> usize {
    match Scause::from_bits(scause).cause() {
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            super::trap::dispatch_timer();
            FaultDecision::Resume as usize
        }
        Trap::Interrupt(Interrupt::SupervisorExternal) => {
            super::trap::dispatch_external();
            FaultDecision::Resume as usize
        }
        Trap::Interrupt(_) => panic!(
            "gateway: unhandled interrupt in component context: scause={:#x}, stval={:#x}",
            scause, stval
        ),
        Trap::Exception(_) => {
            let address = FAULT_HANDLER.load(Ordering::Acquire);
            if address == 0 {
                // 没有 Core 钩子 = 没有任何"可恢复"的证明：保持 fatal。
                panic!(
                    "gateway: component fault with no Core fault handler: scause={:#x}, stval={:#x}",
                    scause, stval
                );
            }
            // SAFETY: 注册方（Core）保证签名与 `ComponentFaultHandler` 一致。
            let handler: ComponentFaultHandler = unsafe { core::mem::transmute(address) };
            handler(frame, scause, stval, token) as usize
        }
    }
}

/// 过渡窗口 / 非组件上下文 trap 的 fatal 出口（汇编调用；不必返回）。
#[unsafe(no_mangle)]
extern "C" fn gateway_fatal(phase: usize, scause: usize, sepc: usize, stval: usize) -> ! {
    super::console::write_fmt(format_args!(
        "\n[gateway] FATAL: trap outside component context: phase={}, scause={:#x}, sepc={:#x}, stval={:#x}\n",
        phase, scause, sepc, stval
    ));
    panic!(
        "gateway trap outside component context (scause={:#x}, sepc={:#x})",
        scause, sepc
    );
}
