//! 输出骨架 + 失败位图：核内测试**唯一**的打印与计数入口。
//!
//! 输出契约（`tests/qemu/runner.py`）：
//! - runner 用**连续子串**匹配 `[core-test] all: PASS`，且把 "FAIL" 当 fatal
//!   marker —— 颜色码只能包在标记之外，绝不能插进标记内部；
//! - 每项检查一行：`[core-test]   <name>: PASS|FAIL`（缩进属于组）；
//! - 汇总：`[core-test]   <passed>/<total> checks PASS`（全过绿，否则红）。
//!
//! [`Checks`] 在 [`Reporter`] 之上叠加实例 create 入口的失败位图：位号是组件内部
//! 约定（runner 只看返回值是否为 0），追加新检查用新位、不复用旧位。

use kcomp_sdk::abi::kcore_console_write_byte;

/// 输出样式（ANSI SGR；终端解释颜色，日志里是可剥离的控制字节）。
mod style {
    pub const RESET: &[u8] = b"\x1b[0m";
    pub const BOLD_RED: &[u8] = b"\x1b[1;31m";
    pub const BOLD_GREEN: &[u8] = b"\x1b[1;32m";
    pub const BOLD_CYAN: &[u8] = b"\x1b[1;36m";
}

fn puts(s: &str) {
    for &b in s.as_bytes() {
        unsafe {
            kcore_console_write_byte(b);
        }
    }
}

fn puts_bytes(bytes: &[u8]) {
    for &b in bytes {
        unsafe {
            kcore_console_write_byte(b);
        }
    }
}

/// 颜色包裹输出：`<color><text><reset>`。
fn puts_colored(color: &[u8], text: &str) {
    puts_bytes(color);
    puts(text);
    puts_bytes(style::RESET);
}

/// 十进制输出（无 alloc 的极简实现，汇总计数用）。
fn put_usize(mut n: usize) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    puts_bytes(&buf[i..]);
}

/// 测试报告器：分组 + 计数 + 颜色（PASS 绿 / FAIL 红）。
struct Reporter {
    passed: usize,
    total: usize,
}

impl Reporter {
    const fn new() -> Self {
        Self {
            passed: 0,
            total: 0,
        }
    }

    /// 组头：`[core-test] ── <title> ──`（青色加粗，整段一次着色）。
    fn group(&mut self, title: &str) {
        puts("[core-test] ");
        puts_bytes(style::BOLD_CYAN);
        puts("── ");
        puts(title);
        puts(" ──");
        puts_bytes(style::RESET);
        puts("\n");
    }

    /// 一项检查：`[core-test]   <name>: PASS|FAIL`（缩进属于组）。返回是否通过。
    fn check(&mut self, name: &str, ok: bool) -> bool {
        puts("[core-test]   ");
        puts(name);
        puts(": ");
        if ok {
            puts_colored(style::BOLD_GREEN, "PASS");
        } else {
            puts_colored(style::BOLD_RED, "FAIL");
        }
        puts("\n");
        self.total += 1;
        if ok {
            self.passed += 1;
        }
        ok
    }

    /// 汇总计数：`[core-test]   <passed>/<total> checks PASS`（全过绿，否则红）。
    fn summary(&self) -> bool {
        let all_ok = self.passed == self.total;
        puts("[core-test]   ");
        put_usize(self.passed);
        puts("/");
        put_usize(self.total);
        puts(" checks ");
        if all_ok {
            puts_colored(style::BOLD_GREEN, "PASS");
        } else {
            puts_colored(style::BOLD_RED, "FAIL");
        }
        puts("\n");
        all_ok
    }

    /// 终判行：**必须**保持 `[core-test] all: PASS` 为连续子串
    /// （runner 标记）。颜色码放在行首/行尾，标记本体不动。
    fn verdict(&self, ok: bool) {
        puts_bytes(if ok {
            style::BOLD_GREEN
        } else {
            style::BOLD_RED
        });
        puts(if ok {
            "[core-test] all: PASS"
        } else {
            "[core-test] all: FAIL"
        });
        puts_bytes(style::RESET);
        puts("\n");
    }
}

/// 检查器：Reporter + 实例 create 入口的失败位图。
pub struct Checks {
    report: Reporter,
    failed: u64,
}

impl Checks {
    pub const fn new() -> Self {
        Self {
            report: Reporter::new(),
            failed: 0,
        }
    }

    pub fn group(&mut self, title: &str) {
        self.report.group(title);
    }

    /// 记一项检查；失败时把 `bit` 并进失败位图（`1 << bit`）。
    ///
    /// 检查数超过 32 后位图升为 `u64`；返回 `i32` 时高 32 位折回低位（见
    /// [`Checks::finish`]），位号约定仍是"追加新检查用新位、不复用旧位"。
    pub fn check(&mut self, bit: u32, name: &str, ok: bool) {
        if !self.report.check(name, ok) {
            self.failed |= 1u64 << bit;
        }
    }

    /// 汇总 + 终判；返回失败位图（0 = 全部通过）。
    ///
    /// create 入口的返回类型是 `i32`，位图高 32 位折回低位（位号只用于定位
    /// 失败检查，`load` 与 runner 只判 0 / 非 0；明细在报告行）。
    pub fn finish(&mut self) -> i32 {
        self.report.group("summary");
        let all_ok = self.report.summary() && self.failed == 0;
        self.report.verdict(all_ok);
        let folded = (self.failed | (self.failed >> 32)) as u32;
        folded as i32
    }
}
