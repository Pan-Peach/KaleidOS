//! PLIC（Platform-Level Interrupt Controller）机制 —— C6 骨架。
//!
//! # 定位
//!
//! `docs/architecture/overview.md` §3：中断控制器（PLIC）的长期定位是**驱动**，由
//! Machine Discovery 发现后作为 Driver Component 运行。现阶段（C6 起步）先把
//! 寄存器机制放在 arch，由 boot 从 discovery 拿到的基址配置；Core 只经
//! `InterruptController` trait 依赖。
//!
//! # 寄存器布局（QEMU virt：`riscv,plic0` / `sifive,plic-1.0.0`）
//!
//! - priority：`base + 4*id`（id ≥ 1；0 = 从不中断 → 必须 ≥ 1）
//! - pending：`base + 0x1000`
//! - enable：`base + 0x2000 + context*0x80`（32 个 u32：word=id/32、bit=id%32）
//! - threshold / claim / complete：`base + 0x200000 + context*0x1000`
//!   （threshold @+0；claim = 读、complete = 写回，同一地址 @+4）
//!
//! **context 来自 boot，不在 S-mode 读 `mhartid`**：`mhartid` 是 M-mode CSR，
//! S-mode 访问是非法指令；OpenSBI 把 hartid 放在 `a0` 传进来、且只清 `sscratch`
//! 不替我们设 `tp`。所以 `configure` 把 boot 已知的 hart 记下来，`context()` 用它
//! 算 `hart*2 + (S模式?1:0)`。
//!
//! # 明确砍掉
//!
//! 优先级配置（本实现写死 1）、触发方式（level/edge）、多 context / SMP affinity、MSI。

use crate::InterruptController;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::Riscv;

/// PLIC MMIO 基址（`configure` 写入；claim/enable/complete 读）。
static PLIC_BASE: AtomicUsize = AtomicUsize::new(0);
/// 本 hart 的 PLIC context 编号基准（`configure` 写入）。
static PLIC_HART: AtomicUsize = AtomicUsize::new(0);

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

/// 当前中断 context：S-mode = hart*2+1；M-mode profile = hart*2。
fn context() -> usize {
    let hart = PLIC_HART.load(Ordering::Acquire);
    #[cfg(feature = "supervisor")]
    {
        hart * 2 + 1
    }
    #[cfg(feature = "machine")]
    {
        hart * 2
    }
}

impl InterruptController for Riscv {
    fn configure(base: usize, hart_id: usize) {
        assert!(base != 0, "PLIC has no base address");
        PLIC_BASE.store(base, Ordering::Release);
        PLIC_HART.store(hart_id, Ordering::Release);
    }

    fn enable(line: u32) {
        let base = base();
        let id = line as usize;

        // priority >= 1, 且要 > threshold（默认 0）才能触发中断
        unsafe { core::ptr::write_volatile((base + PRIORITY_BASE + id * 4) as *mut u32, 1) };

        let ctx = context();
        // 阈值显式写0, 只要 priority >= 1 就能触发中断
        unsafe {
            core::ptr::write_volatile(
                (base + CONTEXT_BASE + ctx * CONTEXT_STRIDE + CONTEXT_THRESHOLD) as *mut u32,
                0,
            )
        };
        let word = (base + ENABLE_BASE + ctx * ENABLE_STRIDE + (id / 32) * 4) as *mut u32;
        let bit = 1u32 << (id % 32);
        unsafe { core::ptr::write_volatile(word, core::ptr::read_volatile(word) | bit) };
    }

    fn disable(line: u32) {
        let base = base();
        let id = line as usize;
        let word = (base + ENABLE_BASE + context() * ENABLE_STRIDE + (id / 32) * 4) as *mut u32;
        let bit = 1u32 << (id % 32);
        unsafe { core::ptr::write_volatile(word, core::ptr::read_volatile(word) & !bit) };
    }

    fn claim() -> Option<u32> {
        let addr = (base() + CONTEXT_BASE + context() * CONTEXT_STRIDE + CONTEXT_CLAIM) as *mut u32;
        let id = unsafe { core::ptr::read_volatile(addr) };
        (id != 0).then_some(id)
    }

    fn complete(line: u32) {
        let addr = (base() + CONTEXT_BASE + context() * CONTEXT_STRIDE + CONTEXT_CLAIM) as *mut u32;
        unsafe { core::ptr::write_volatile(addr, line) };
    }

    fn register_external_handler(handler: extern "C" fn()) {
        super::trap::register_external_handler(handler);
    }

    fn enable_external_interrupt() {
        super::firmware::enable_external_interrupt();
    }
}
