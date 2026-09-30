//! 核心日志（启动/诊断/panic 的早期输出）。
//!
//! 分层：格式化在 core（`printk!`/`log!` 宏 → `print::print`），传输通过
//! arch crate 的 `Console` backend。Core 不依赖具体 SBI、UART 或 std 实现。
//!
//! - `printk!`：无前缀输出（裸打印）
//! - `log!(tag, ...)`：`[tag] ...\n`（Linux dmesg 风格）
//! - `print`：`printk!` 宏的底层（接受 fmt::Arguments）
//!
//! host 测试：Fake console 写 stdout，printk! 自然可见。
//! 裸机：当前 RISC-V console 走 SBI。
//!
//! **注意**：panic 路径不走本模块 —— panic 可能发生在锁/堆损坏时，
//! 由 bootstrap 的静态紧急 console 直连输出（见 bootstrap console.rs）。

use crate::monitor::editor::{LineEditor, Outcome, Screen};
use arch::{Console, ConsoleImpl, Timer, TimerImpl};
use core::fmt::{self, Write};

/// 无前缀输出（core 内部各模块的自描述日志用）。
/// 传输到当前 `Console` backend（逐字节）。
pub fn print(args: fmt::Arguments<'_>) {
    let mut sink = Sink;
    let _ = sink.write_fmt(args);
}

/// 带前缀日志：`[tag] args\n`。
pub fn log(tag: &str, args: fmt::Arguments<'_>) {
    let mut sink = Sink;
    let _ = write!(sink, "[{}] ", tag);
    let _ = sink.write_fmt(args);
    let _ = sink.write_str("\n");
}

/// 打印任意字节序列（不经格式化，monitor 回显/原始输出用）。
///
/// 逐字节原样写 Console backend：未知命令回显必须是**精确字节**，
/// 不做 UTF-8 校验/替换。
pub fn print_bytes(bytes: &[u8]) {
    for &byte in bytes {
        ConsoleImpl::write_byte(byte);
    }
}

/// 屏幕 sink：把行编辑器的输出原样接到 Console backend。
pub(crate) struct ConsoleScreen;

impl Screen for ConsoleScreen {
    fn put(&mut self, bytes: &[u8]) {
        print_bytes(bytes);
    }
}

// ---------------------------------------------------------------------------
// Monitor 输入：read_line（行编辑器 + 轮询 Console::getc）
// ---------------------------------------------------------------------------

/// 读一行（编辑器无提示符、无补全）。返回提交的字节数；空行 / Ctrl-C /
/// Ctrl-D 返回 0。
///
/// 保留本函数给 boot selftest（单次调用、64 字节缓冲）——monitor 主循环
/// 另有带提示符/补全的接线。
pub fn read_line(buf: &mut [u8]) -> usize {
    let mut editor = LineEditor::new();
    let mut screen = ConsoleScreen;
    loop {
        match ConsoleImpl::getc() {
            Some(byte) => match editor.feed("", byte, &[], &mut screen) {
                Outcome::Pending => {}
                Outcome::Submitted => {
                    let line = editor.line();
                    let len = line.len().min(buf.len());
                    buf[..len].copy_from_slice(&line[..len]);
                    return len;
                }
                Outcome::Cancelled | Outcome::Eof => return 0,
            },
            None => idle_wait(),
        }
    }
}

/// 空闲等待：「检查-睡眠原子化」的短 one-shot timer + CPU idle（唤醒后回到
/// getc 轮询）。
///
/// `Console::getc()` 是轮询式调用（未使能 UART RX 中断），单独 idle 没有东西
/// 能唤醒——所以先关本 CPU 中断，在**关中断状态下** arm 一个 ~10ms 的
/// one-shot deadline，再用 `atomic_idle(saved_flags)` 进入 idle：
/// - 唤醒源与谓词检查在同一临界区内建立，不存在"检查完就睡、事件被吞"的窗口；
/// - `atomic_idle` 返回前恢复中断状态。
///
/// **永不挂死**：频率未知 / 投递未就绪 / arm 失败时不睡眠，回退到轮询自旋。
/// `delivery_ready()` 与 `arm_deadline()` 在机制初始化前都不分配。
pub(crate) fn idle_wait() {
    // 频率未知（如 x86 的 0 = unknown 约定）或换算失败：回退轮询。
    let Some(period) = idle_period() else {
        core::hint::spin_loop();
        return;
    };
    // readiness 只从 false 单调变 true；这里提前判掉"没有投递"的情形，
    // 避免无意义地翻转中断状态（arm_deadline 仍会二次校验）。
    if !crate::timer::delivery_ready() {
        core::hint::spin_loop();
        return;
    }
    let flags = <arch::CpuImpl as arch::CpuArch>::disable_irq();
    let deadline = TimerImpl::now().saturating_add(period);
    if crate::timer::arm_deadline(deadline).is_ok() {
        // SAFETY: local IRQs are disabled (above); the one-shot deadline was
        // armed while they were disabled, so the wakeup cannot be lost between
        // the check and the sleep; no interrupt-path lock is held.
        unsafe { <arch::CpuImpl as arch::CpuArch>::atomic_idle(flags) };
    } else {
        // 编程失败：恢复中断，轮询。
        <arch::CpuImpl as arch::CpuArch>::restore_irq(flags);
        core::hint::spin_loop();
    }
}

/// idle 周期（tick）：只从已提交 `MachineInfo` 的**已知非零** timebase 频率
/// 换算 ~10ms。频率未知（如 x86 的 0 = unknown 约定）或换算截断为 0 → `None`
/// （调用者回退轮询）——**不伪造**任何架构专属常量。
fn idle_period() -> Option<u64> {
    const TARGET_MS: u64 = 10;
    let info = crate::machine::committed()?;
    let hz = info.timebase_frequency;
    if hz == 0 {
        return None;
    }
    let period = hz / (1000 / TARGET_MS);
    (period != 0).then_some(period)
}

// ---------------------------------------------------------------------------
// Sink：把格式化结果逐个字节送给 Console backend
// ---------------------------------------------------------------------------

struct Sink;

impl Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            ConsoleImpl::write_byte(byte);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 宏：printk!（无前缀）/ log!（显式 tag）
// ---------------------------------------------------------------------------

/// 裸打印（无前缀、不自动加换行）：
/// printk!("core> ");
/// printk!("region {:#x}\n", addr);
#[macro_export]
macro_rules! printk {
    ($($arg:tt)*) => {
        $crate::print::print(core::format_args!($($arg)*))
    };
}

/// 带 tag 日志（Linux dmesg 风格）：
/// log!("memory", "init OK");
/// log!("memory", "region {:#x}", addr);
#[macro_export]
macro_rules! log {
    ($tag:expr, $($arg:tt)*) => {
        $crate::print::log($tag, core::format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 安装一份指定 timebase 频率的机器 fixture（只关心 idle 换算字段）。
    ///
    /// 全局快照是进程级读路径，调用方必须持有 `machine::test_support::GUARD`。
    fn install_timebase(timebase_frequency: u64) {
        let info = crate::machine::test_support::snapshot(
            crate::machine::HardwareCpuId::from_raw(0),
            timebase_frequency,
            alloc::vec![crate::machine::CpuInfo {
                boot_cpu: true,
                hardware_id: crate::machine::HardwareCpuId::from_raw(0),
            }],
            alloc::vec![crate::machine::MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }],
            alloc::vec![],
        );
        crate::machine::test_support::install(info);
    }

    /// `idle_period()` 用已提交 timebase 换算 ~10ms：10MHz → 100_000 ticks。
    #[test]
    fn idle_period_converts_committed_timebase_to_ten_milliseconds() {
        let _guard = crate::machine::test_support::GUARD.lock();

        // Given：已提交 10 MHz timebase（QEMU virt 典型值）。
        install_timebase(10_000_000);

        // When：计算 idle 周期。
        let period = idle_period();

        // Then：10ms @ 10MHz = 100_000 ticks。
        assert_eq!(period, Some(100_000), "10ms @ 10MHz 应换算为 100_000 ticks");
    }

    /// timebase 太小导致整除截断为 0 时不得伪造常量：返回 `None`（调用者轮询）。
    #[test]
    fn idle_period_is_unknown_when_timebase_truncates_to_zero() {
        let _guard = crate::machine::test_support::GUARD.lock();

        // Given：50 Hz timebase → 50 / 100 == 0。
        install_timebase(50);

        // When：计算 idle 周期。
        let period = idle_period();

        // Then：未知（绝不回退到架构专属常量）。
        assert_eq!(period, None, "截断为 0 时必须报未知而不是伪造周期");
    }

    /// timebase 频率为 0（如 x86 的 zero-as-unknown 约定）→ 未知，回退轮询。
    #[test]
    fn idle_period_is_unknown_when_timebase_frequency_is_zero() {
        let _guard = crate::machine::test_support::GUARD.lock();

        // Given：timebase 频率未知（0）。
        install_timebase(0);

        // When：计算 idle 周期。
        let period = idle_period();

        // Then：未知（回退轮询，绝不伪造）。
        assert_eq!(period, None, "timebase=0 是未知约定，必须返回 None");
    }
}
