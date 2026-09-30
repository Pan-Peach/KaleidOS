//! PLIC（Platform-Level Interrupt Controller）机制 —— C6 骨架 + SMP 契约。
//!
//! # SMP 契约
//!
//! - `configure` 只做**全局**配置，携带**逻辑 CPU → PLIC context 的映射表**；
//!   `hart*2+privilege` 是板级假设，由 boot 算出后填表（arch 不再写死）。
//! - `init_cpu` 选择**当前执行 CPU** 的 context，供 claim/complete 用。
//! - `enable/disable` 作用于**固定路由**（`external_cpu`）的 context，不是调用者
//!   CPU；且对 enable bank 的读改写全程持锁 + irq-save，避免两个 CPU 改同一字丢更新。
//! - `claim/complete` 是**后端私有**的（不是 trait 方法）：[`PlicClaim`]（携带
//!   line + context，非 `Copy`/`Send`）与 CPU-context 逻辑都留在本模块。
//!
//! # 与 Core 的边界
//!
//! 后端拥有 ack/EOI 与源映射：`configure` 把私有分发器
//! [`dispatch_external`] 装进 `trap::register_external_handler`；它在一次
//! trap 里循环 `claim → Core 回调（逻辑 IRQ 号）→ complete`，直到无 pending。
//! Core 只注册 [`crate::cpu::ExternalIrqHandler`] 并按逻辑 IRQ 号路由，
//! 看不到 claim 令牌 / context / 寄存器（见 `trait InterruptController` 文档）。
//!
//! # 明确砍掉
//!
//! 优先级配置（写死 1）、触发方式、IRQ 均衡（固定路由到 BSP）、MSI。

use crate::InterruptController;
use crate::cpu::{CpuId, ExternalIrqHandler};
use crate::smp;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::Riscv;

/// 配置能表达的 PLIC context 上限。
///
/// 每个 CPU 至少一个 context，所以上限 = 编译期 CPU 容量
/// （`arch::MAX_CPUS`，Kconfig `MAX_CPUS`）。
pub const MAX_PLIC_CONTEXTS: usize = crate::MAX_CPUS;

/// 一个逻辑 CPU 对应的 PLIC context 编号。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlicCpuContext {
    /// Core 赋予的逻辑 CPU 身份。
    pub cpu: CpuId,
    /// 该 CPU 在本平台的 PLIC context 编号（板级计算，boot 填充）。
    pub context: usize,
}

/// PLIC 全局配置，**由 boot 构造**。
pub struct PlicConfig {
    /// PLIC MMIO 基址。
    pub base: usize,
    /// 逻辑 CPU → context 映射（只用前 `context_count` 项）。
    pub contexts: [PlicCpuContext; MAX_PLIC_CONTEXTS],
    /// 有效映射项数。
    pub context_count: usize,
    /// 外部设备线固定路由到的逻辑 CPU（初期 BSP）。
    pub external_cpu: CpuId,
    /// 控制器支持的源数量上界（用于 enable 越界校验）。
    pub source_count: u32,
}

/// PLIC claim 令牌：保留 complete 所需的完整信息；非 `Copy` / 非 `Send`。
///
/// **后端私有**：Core 永远看不到它（Core 只收逻辑 IRQ 号）。
struct PlicClaim {
    line: u32,
    context: usize,
    _cpu_local: PhantomData<*mut ()>,
}

static PLIC_BASE: AtomicUsize = AtomicUsize::new(0);
static PLIC_CONTEXT_COUNT: AtomicUsize = AtomicUsize::new(0);
static PLIC_EXTERNAL_CPU: AtomicUsize = AtomicUsize::new(0);
static PLIC_SOURCE_COUNT: AtomicUsize = AtomicUsize::new(0);
/// 已注册的 Core 外部中断回调（`0` = 未注册；Core `irq::init` 注册一次）。
static PLIC_CORE_HANDLER: AtomicUsize = AtomicUsize::new(0);
/// enable bank 读改写的锁（与 irq-save 配合）。
static PLIC_ENABLE_LOCK: AtomicBool = AtomicBool::new(false);

/// 配置期写入的映射表（configure 一次；之后只读）。与 `trap/mod.rs` 的
/// `TRAP_STACK` 同一模式：boot 单线程写完，再用 `PLIC_CONTEXT_COUNT` 发布。
static mut PLIC_CONTEXTS: [PlicCpuContext; MAX_PLIC_CONTEXTS] = [PlicCpuContext {
    cpu: CpuId::from_raw(0),
    context: 0,
}; MAX_PLIC_CONTEXTS];

const PRIORITY_BASE: usize = 0x000000;
const ENABLE_BASE: usize = 0x002000;
const ENABLE_STRIDE: usize = 0x80; // 每 context 32 个 u32
const CONTEXT_BASE: usize = 0x200000;
const CONTEXT_STRIDE: usize = 0x1000;
const CONTEXT_THRESHOLD: usize = 0x00;
const CONTEXT_CLAIM: usize = 0x04;

fn base() -> usize {
    let base = PLIC_BASE.load(Ordering::Acquire);
    assert!(base != 0, "PLIC not configured");
    base
}

/// 配置期写入的映射表只读视图。
fn contexts() -> &'static [PlicCpuContext] {
    let len = PLIC_CONTEXT_COUNT.load(Ordering::Acquire);
    // SAFETY: `PLIC_CONTEXTS` 在 configure（boot 单线程）后不再修改；len 由同一
    // configure 发布且 <= MAX_PLIC_CONTEXTS。
    let all: &'static [PlicCpuContext; MAX_PLIC_CONTEXTS] =
        unsafe { &*core::ptr::addr_of!(PLIC_CONTEXTS) };
    &all[..len.min(MAX_PLIC_CONTEXTS)]
}

fn context_of(cpu: CpuId) -> Option<usize> {
    contexts().iter().find(|c| c.cpu == cpu).map(|c| c.context)
}

/// **当前执行 CPU** 的 claim/complete context。
fn cpu_context() -> usize {
    use crate::CpuArch;
    let cpu = Riscv::current_cpu().expect("current CPU is not bound during PLIC claim");
    context_of(cpu).expect("current CPU's PLIC context is not configured")
}

/// enable/disable 作用的固定路由 context。
fn external_context() -> usize {
    let cpu = CpuId::from_raw(PLIC_EXTERNAL_CPU.load(Ordering::Acquire));
    context_of(cpu).expect("external-routed CPU's PLIC context is not configured")
}

fn enable_word(id: usize, context: usize) -> *mut u32 {
    (base() + ENABLE_BASE + context * ENABLE_STRIDE + (id / 32) * 4) as *mut u32
}

/// enable bank 读改写：irq-save + 自旋锁，禁止两个 CPU 丢更新。
fn with_enable_lock<R>(f: impl FnOnce() -> R) -> R {
    use crate::CpuArch;
    let flags = Riscv::disable_irq();
    while PLIC_ENABLE_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let result = f();
    PLIC_ENABLE_LOCK.store(false, Ordering::Release);
    Riscv::restore_irq(flags);
    result
}

/// 取一条 pending 外部中断（PLIC claim）；无 pending → `None`。
///
/// **后端私有**：令牌（line + context）只在 `dispatch_external` 与 [`complete`]
/// 之间传递，Core 拿不到。
fn claim() -> Option<PlicClaim> {
    let context = cpu_context();
    let addr = (base() + CONTEXT_BASE + context * CONTEXT_STRIDE + CONTEXT_CLAIM) as *mut u32;
    let id = unsafe { core::ptr::read_volatile(addr) };
    (id != 0).then_some(PlicClaim {
        line: id,
        context,
        _cpu_local: PhantomData,
    })
}

/// 通知控制器该中断已处理（PLIC complete 写回 claim id）。
fn complete(claim: PlicClaim) {
    let addr = (base() + CONTEXT_BASE + claim.context * CONTEXT_STRIDE + CONTEXT_CLAIM) as *mut u32;
    unsafe { core::ptr::write_volatile(addr, claim.line) };
}

/// 后端外部中断分发器（`configure` 装进 `trap::register_external_handler`）。
///
/// 一次 trap 可能对应多条 pending（PLIC 共享一个 mip 位）：循环
/// `claim → Core 回调（逻辑 IRQ 号）→ complete` 直到无 pending。与 Core 的
/// 唯一接触面是 [`ExternalIrqHandler`]；令牌 / context / 寄存器都不外泄。
fn dispatch_external(cpu: CpuId) {
    let address = PLIC_CORE_HANDLER.load(Ordering::Acquire);
    assert!(address != 0, "external interrupt handler is not registered");
    // SAFETY: 注册方保证签名与 `ExternalIrqHandler` 一致（单一注册入口）。
    let core: ExternalIrqHandler = unsafe { core::mem::transmute(address) };
    while let Some(claim) = claim() {
        let irq = claim.line;
        core(cpu, irq);
        complete(claim);
    }
}

impl InterruptController for Riscv {
    type Config = PlicConfig;

    unsafe fn configure(config: Self::Config) -> Result<(), smp::InitError> {
        if config.base == 0
            || config.context_count == 0
            || config.context_count > MAX_PLIC_CONTEXTS
            || config.source_count == 0
            || context_of_in(&config, config.external_cpu).is_none()
        {
            return Err(smp::InitError::InvalidConfiguration);
        }
        // 逻辑 id 不得重复。
        for i in 0..config.context_count {
            for j in (i + 1)..config.context_count {
                if config.contexts[i].cpu == config.contexts[j].cpu {
                    return Err(smp::InitError::InvalidConfiguration);
                }
            }
        }
        // SAFETY: boot 单线程；写完再置 count 发布，之后只读。
        unsafe {
            let dst = core::ptr::addr_of_mut!(PLIC_CONTEXTS).cast::<PlicCpuContext>();
            for i in 0..config.context_count {
                dst.add(i).write(config.contexts[i]);
            }
        }
        PLIC_SOURCE_COUNT.store(config.source_count as usize, Ordering::Release);
        PLIC_EXTERNAL_CPU.store(config.external_cpu.raw(), Ordering::Release);
        PLIC_BASE.store(config.base, Ordering::Release);
        PLIC_CONTEXT_COUNT.store(config.context_count, Ordering::Release);
        // 安装后端 trap 分发器（本函数在 boot 单线程跑一次）：`claim → Core
        // 回调（逻辑号）→ complete` 的循环完全留在后端；Core 只注册回调。
        super::trap::register_external_handler(dispatch_external);
        Ok(())
    }

    fn init_cpu() -> Result<(), smp::InitError> {
        // 选择本 CPU 的 context（按 `current_cpu` 查表；UP 恒为 CPU0），并解开本
        // CPU 的外部中断**投递源**；全局使能由 `CpuArch::enable_irq` 单独负责。
        let _ = cpu_context();
        super::firmware::enable_external_interrupt();
        Ok(())
    }

    fn enable(line: u32) {
        let id = line as usize;
        assert!(
            id < PLIC_SOURCE_COUNT.load(Ordering::Acquire),
            "PLIC enable out of range"
        );
        let ctx = external_context();
        with_enable_lock(|| {
            // priority >= 1，且要 > threshold（默认 0）才能触发
            unsafe { core::ptr::write_volatile((base() + PRIORITY_BASE + id * 4) as *mut u32, 1) };
            unsafe {
                core::ptr::write_volatile(
                    (base() + CONTEXT_BASE + ctx * CONTEXT_STRIDE + CONTEXT_THRESHOLD) as *mut u32,
                    0,
                )
            };
            let word = enable_word(id, ctx);
            let bit = 1u32 << (id % 32);
            unsafe { core::ptr::write_volatile(word, core::ptr::read_volatile(word) | bit) };
        });
    }

    fn disable(line: u32) {
        let id = line as usize;
        let ctx = external_context();
        with_enable_lock(|| {
            let word = enable_word(id, ctx);
            let bit = 1u32 << (id % 32);
            unsafe { core::ptr::write_volatile(word, core::ptr::read_volatile(word) & !bit) };
        });
    }

    fn register_external_handler(handler: ExternalIrqHandler) {
        PLIC_CORE_HANDLER.store(handler as usize, Ordering::Release);
    }

    fn enable_external_interrupt() {
        super::firmware::enable_external_interrupt();
    }
}

fn context_of_in(config: &PlicConfig, cpu: CpuId) -> Option<usize> {
    config.contexts[..config.context_count]
        .iter()
        .find(|c| c.cpu == cpu)
        .map(|c| c.context)
}
