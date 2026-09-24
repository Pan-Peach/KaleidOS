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
use arch::{Console, ConsoleImpl, CpuArch, Timer, TimerImpl};
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

/// 空闲等待：短 one-shot timer + WFI（唤醒后回到 getc 轮询）。
///
/// `Console::getc()` 是轮询式 SBI 调用（未使能 UART RX 中断），单独 `wfi`
/// 没有任何东西能唤醒——所以先 arm 一个 ~10ms 的 one-shot deadline，让
/// timer IRQ 把 WFI 叫醒。arm 失败（timer 未初始化，如 selftest 早于
/// `core::init`）退回自旋，**绝不挂死**。
pub(crate) fn idle_wait() {
    let now = TimerImpl::now();
    let deadline = now.saturating_add(idle_period());
    if crate::timer::arm_deadline(deadline).is_ok() {
        arch::CpuImpl::wait_for_interrupt();
    } else {
        core::hint::spin_loop();
    }
}

/// ~10ms 的 idle 周期：优先用已提交 MachineInfo 的 timebase 频率换算，
/// 缺省用安全常量（10ms @ 10MHz，QEMU virt 的 timebase）。
fn idle_period() -> u64 {
    const TARGET_MS: u64 = 10;
    const FALLBACK_TICKS: u64 = 100_000;
    let Some(info) = crate::machine::committed() else {
        return FALLBACK_TICKS;
    };
    let period = info.timebase_frequency / (1000 / TARGET_MS);
    if period == 0 { FALLBACK_TICKS } else { period }
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

    /// 提交一份指定 timebase 频率的 `MachineInfo`（只关心 idle 换算字段）。
    ///
    /// `COMMITTED` 是进程全局，调用方必须持有 `machine::test_support::GUARD`。
    fn commit_timebase(timebase_frequency: u64) {
        crate::machine::commit(crate::machine::MachineInfo {
            boot_hart: 0,
            timebase_frequency,
            cpu_count: 1,
            cpu_info: [crate::machine::CpuInfo {
                boot_cpu: true,
                hart_id: crate::machine::CpuId::from_raw(0),
            }; 8],
            mem_count: 1,
            memory_regions: [crate::machine::MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }; 16],
            dev_count: 0,
            devices: [crate::machine::DeviceDescriptor::empty(); 26],
        });
    }

    /// `idle_period()` 用已提交 timebase 换算 ~10ms：10MHz → 100_000 ticks。
    #[test]
    fn idle_period_converts_committed_timebase_to_ten_milliseconds() {
        let _guard = crate::machine::test_support::GUARD.lock();

        // Given：已提交 10 MHz timebase（QEMU virt 典型值）。
        commit_timebase(10_000_000);

        // When：计算 idle 周期。
        let period = idle_period();

        // Then：10ms @ 10MHz = 100_000 ticks。
        assert_eq!(period, 100_000, "10ms @ 10MHz 应换算为 100_000 ticks");
    }

    /// timebase 太小导致整除截断为 0 时必须回退到安全常量，绝不返回 0。
    #[test]
    fn idle_period_falls_back_when_timebase_truncates_to_zero() {
        let _guard = crate::machine::test_support::GUARD.lock();

        // Given：50 Hz timebase → 50 / 100 == 0。
        commit_timebase(50);

        // When：计算 idle 周期。
        let period = idle_period();

        // Then：回退到安全常量（100_000），不得为 0。
        assert_eq!(period, 100_000, "截断为 0 时必须回退到 100_000");
        assert_ne!(period, 0, "idle 周期绝不能为 0");
    }
}
