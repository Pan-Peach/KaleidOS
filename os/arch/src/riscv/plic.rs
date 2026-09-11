//! PLIC（Platform-Level Interrupt Controller）机制 —— C6 骨架。
//!
//! # 定位
//!
//! `docs/architecture.md` §3：中断控制器（PLIC）的长期定位是**驱动**，由
//! Machine Discovery 发现后作为 Driver Component 运行。现阶段（C6 起步）先把
//! 寄存器机制放在 arch，由 boot 从 discovery 拿到的基址配置；Core 只经
//! `InterruptController` trait 依赖。未来降级为驱动时 Core 侧调用点不变。
//!
//! # 实现要点（TODO：寄存器逻辑待手写）
//!
//! QEMU virt（compatible `riscv,plic0` / `sifive,plic-1.0.0`）：
//! - priority：`base + 4*line`（line ≥ 1；0 = 从不中断）
//! - pending：`base + 0x1000`
//! - enable：`base + 0x2000 + context*0x80`（S-mode context = hart*2 + 1）
//! - threshold / claim / complete：`base + 0x200000 + context*0x1000`
//!   （claim = 读回 pending 的中断号；complete = 写回同一中断号）
//!
//! boot 只调用 `configure(base)`；其余留给实现。
//!
//! # 明确砍掉
//!
//! 优先级配置、触发方式（level/edge）、多 context / SMP affinity、MSI。

use crate::InterruptController;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::Riscv;

/// PLIC MMIO 基址（`configure` 写入；claim/enable/complete 读）。
static PLIC_BASE: AtomicUsize = AtomicUsize::new(0);

impl InterruptController for Riscv {
    fn configure(base: usize) {
        assert!(base != 0, "PLIC has no base address");
        PLIC_BASE.store(base, Ordering::Release);
    }

    fn enable(line: u32) {
        let base = PLIC_BASE.load(Ordering::Acquire);
        todo!("C6: PLIC enable line {line} at {base:#x}")
    }

    fn disable(line: u32) {
        let base = PLIC_BASE.load(Ordering::Acquire);
        todo!("C6: PLIC disable line {line} at {base:#x}")
    }

    fn claim() -> Option<u32> {
        let base = PLIC_BASE.load(Ordering::Acquire);
        todo!("C6: PLIC claim at {base:#x}")
    }

    fn complete(line: u32) {
        let base = PLIC_BASE.load(Ordering::Acquire);
        todo!("C6: PLIC complete line {line} at {base:#x}")
    }

    fn register_external_handler(handler: extern "C" fn()) {
        super::trap::register_external_handler(handler);
    }

    fn enable_external_interrupt() {
        super::firmware::enable_external_interrupt();
    }
}
