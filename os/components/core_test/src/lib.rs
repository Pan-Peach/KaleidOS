//! CoreTest 测试组件（第一个 .kcomp）：核内自检 Core 的真实接口。
//!
//! - `kcomp_init`：loader 放段 + 重定位后调用；返回 0 = 全部通过，
//!   非 0 = 失败位图（`load` 命令会据此报告 FAILED）
//! - 报告分组（`runtime/`，每个分组一个模块 —— 模块边界 = 责任边界）：
//!   - `boot`：`.data` 段搬运 / 机器真相 / 内存分配器 / 组件注册表
//!   - `sched`：组件加载 → 接口可用 → 任务创建/启动 → RR 调度 → yield/exit
//!   - `resource`：MMIO / IRQ / DMA 的 discover → claim → access → release，
//!     外加 Core 已明确定义的拒绝路径（重复认领 / 子项未释放 / 死 root 派生）
//!   - `trace`：**操作 → 事件**断言（操作前取游标、用操作返回的 id/handle 精确匹配）
//! - 平台事实不入本组件：QEMU virt 的 PLIC 线号 / context 公式 / enable bit
//!   读回属于平台白盒，已由 ArchTest `external-irq`
//!   （`os/boot/riscv/src/selftest.rs`）直接驱动 PLIC 覆盖。这里只断言 Core
//!   自己报告的返回值与状态编码，不读中断控制器寄存器
//! - 只走导出白名单（`kcomp-sdk` 的 `kcore_*` 声明是 ABI 单一来源），无 god-mode；
//!   不直接触碰 TaskTable / Registry / Sv39 / CpuContext —— 那是 Core 的真相
//! - host 测试只覆盖纯逻辑；真实执行在核内

#![no_std]

// 组件私有 panic adapter（kcomp-sdk）：只提供裸机 #[panic_handler] + 链接期
// Rust support，符号未被引用时被 --gc-sections 丢弃。
use kcomp_sdk as _;

#[cfg(test)]
extern crate std;

/// 核内运行时（host 构建整体编掉：host 上只测纯逻辑，不引用 `kcore_*`）。
#[cfg(not(test))]
mod runtime;

/// 静态数据，.data 段。放段/重定位正确性锚点：值原样保持 = 段被正确搬运。
static MAGIC: u32 = 0xC0FFEE;

/// 纯逻辑（host-testable）：.data 段的值在放段后必须原样可读。
fn data_ok() -> bool {
    MAGIC == 0xC0FFEE
}

/// Validate the machine facts supplied by the Core exports.  The actual hart
/// membership lookup stays in Core; this pure predicate remains host-testable.
fn machine_ok(cpu_count: u32, boot_hart_present: bool) -> bool {
    cpu_count >= 1 && boot_hart_present
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_ok_passes() {
        assert!(data_ok());
    }

    #[test]
    fn machine_check_accepts_sparse_hart_ids() {
        assert!(machine_ok(2, true));
    }

    #[test]
    fn machine_check_rejects_missing_boot_hart() {
        assert!(!machine_ok(2, false));
    }
}
