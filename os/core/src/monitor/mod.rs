//! Core Monitor —— 裸 Core 的交互调试面（只读为主，无组件依赖）。
//!
//! 设计约束（架构定案）：
//! - Monitor 是 Core 常态能力，不是"系统崩了才出现"的救火模式；
//! - 不依赖 Scheduler/Logger/Network 等任何 Component —— 组件全挂也要能跑；
//! - 只读观察系统状态（machine/memory），不提供 god-mode 修改；
//! - 不因拥有 console 而获得 authority（Oracle 审查结论）。
//!
//! 主循环：`core> _` 提示符 → 轮询取字节 → 行编辑（历史/Tab 补全）→
//! token 解析 → 命令表分发 → 循环。空闲时走 `print::idle_wait()`（短
//! one-shot timer + WFI，不忙等烧 CPU）。
//! 输入/输出都走 arch crate 的 Console backend，无注入层。

use crate::monitor::editor::{LineEditor, Outcome, Screen};
use crate::print::{self, ConsoleScreen};
use crate::printk;
use arch::Console;

mod cmds;
pub mod editor;

/// 交互提示符（重绘时重新输出；`LINE_MAX` 与其匹配 80 列预算）。
const PROMPT: &str = "core> ";

/// 命令表：名称 + 执行函数（返回是否已执行；args 预留给多值参数）。
struct Command {
    name: &'static str,
    help: &'static str,
    run: fn(line: &[u8]),
}

const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        help: "this message",
        run: cmds::help,
    },
    Command {
        name: "machine",
        help: "dump MachineInfo",
        run: cmds::machine,
    },
    Command {
        name: "memory",
        help: "physical memory allocator stats",
        run: cmds::memory,
    },
    Command {
        name: "tasks",
        help: "list tasks",
        run: cmds::tasks,
    },
    Command {
        name: "load",
        help: "load a kcomp by name (e.g. load core_test [native])",
        run: cmds::load,
    },
    Command {
        name: "unload",
        help: "stop a kcomp by name (e.g. unload kcomp_smoke)",
        run: cmds::unload,
    },
    Command {
        name: "components",
        help: "list loaded components",
        run: cmds::components,
    },
    Command {
        name: "catalog",
        help: "list loadable components in store",
        run: cmds::catalog,
    },
    Command {
        name: "trace",
        help: "trace status / toggle event kinds",
        run: cmds::trace,
    },
    Command {
        name: "shutdown",
        help: "shutdown the system",
        run: cmds::shutdown,
    },
    Command {
        name: "reboot",
        help: "reboot the system",
        run: cmds::reboot,
    },
];

fn trim_leading(mut s: &[u8]) -> &[u8] {
    while let Some(&b' ' | &b'\t') = s.first() {
        s = &s[1..];
    }
    s
}

/// 命令解析结果：命中命令（含剥离后的参数）或未知（含首个 token）。
enum Resolved<'a> {
    Known(&'static Command, &'a [u8]),
    Unknown(&'a [u8]),
}

/// 行 → (命令, 参数)，无 I/O；run() 负责分发，这里供 host 测试直接调用。
fn resolve_command(line: &[u8]) -> Resolved<'_> {
    let line = line.trim_ascii();
    let first = line
        .split(|&b| b.is_ascii_whitespace())
        .next()
        .unwrap_or(b"");
    match COMMANDS.iter().find(|c| c.name.as_bytes() == first) {
        Some(c) => Resolved::Known(c, trim_leading(&line[first.len()..])),
        None => Resolved::Unknown(first),
    }
}

/// 进入 Monitor 主循环（永不返回）。
///
/// 行编辑状态常驻（历史/Tab 补全跨命令保持）；空闲等待 = 短 timer + WFI，
/// 永不挂死（见 `print::idle_wait`）。
pub fn run() -> ! {
    printk!("KaleidOS Core Monitor\n");
    printk!("type 'help' for commands\n");

    let mut editor = LineEditor::new();
    let mut screen = ConsoleScreen;
    // 补全候选 = 命令名（从 COMMANDS 派生，避免两份表漂移）。
    let mut names = [""; COMMANDS.len()];
    for (slot, cmd) in names.iter_mut().zip(COMMANDS) {
        *slot = cmd.name;
    }

    loop {
        screen.put(PROMPT.as_bytes());
        loop {
            // A yielding console task returns to this anchor while still
            // Runnable. Resume component work before consuming its input.
            // NoPolicy leaves the monitor available for initial composition.
            if crate::sched::service_local() {
                continue;
            }
            match arch::ConsoleImpl::getc() {
                Some(byte) => match editor.feed(PROMPT, byte, &names, &mut screen) {
                    Outcome::Pending => {}
                    Outcome::Submitted => {
                        let line = editor.line();
                        // 空行 / 纯空白：静默重来，
                        // 不能报 unknown command ''。
                        if !line.trim_ascii().is_empty() {
                            match resolve_command(line) {
                                Resolved::Known(cmd, args) => (cmd.run)(args),
                                Resolved::Unknown(first) => {
                                    printk!("unknown command '");
                                    print::print_bytes(first);
                                    printk!("' (try 'help')\n");
                                }
                            }
                        }
                        break;
                    }
                    Outcome::Cancelled => break,
                    // Ctrl-D 空行：Monitor 不退出（run 永不返回），忽略。
                    Outcome::Eof => {}
                },
                None => print::idle_wait(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::string::ToString;

    #[test]
    fn load_underscore_stays_one_token() {
        // `_` (0x5f) is not ASCII whitespace, so `core_test` must stay one
        // token and be handed to the `load` command as a single argument.
        match resolve_command(b"load core_test") {
            Resolved::Known(cmd, args) => {
                assert_eq!(cmd.name, "load");
                assert_eq!(args, b"core_test".as_slice());
            }
            Resolved::Unknown(_) => panic!("expected a Known 'load' command"),
        }
    }

    fn name(res: &Resolved<'_>) -> String {
        match res {
            Resolved::Known(c, _) => c.name.to_string(),
            Resolved::Unknown(tok) => core::str::from_utf8(tok).unwrap_or("<bad>").to_string(),
        }
    }

    fn args<'a>(res: &'a Resolved<'a>) -> &'a [u8] {
        match res {
            Resolved::Known(_, a) => a,
            Resolved::Unknown(_) => b"",
        }
    }

    #[test]
    fn known_command_with_args() {
        let r = resolve_command(b"memory");
        assert_eq!(name(&r), "memory");
        assert!(args(&r).is_empty());
    }

    #[test]
    fn multiple_spaces_are_stripped() {
        let r = resolve_command(b"memory    ");
        assert_eq!(name(&r), "memory");
        assert!(args(&r).is_empty());
    }

    #[test]
    fn command_only_has_empty_args() {
        let r = resolve_command(b"memory");
        assert_eq!(name(&r), "memory");
        assert!(args(&r).is_empty());
    }

    #[test]
    fn ascii_whitespace_separates_command_and_args() {
        let r = resolve_command(b"memory\t0x1000");
        assert_eq!(name(&r), "memory");
        assert_eq!(args(&r), b"0x1000");
    }

    #[test]
    fn leading_and_trailing_whitespace_is_ignored() {
        let r = resolve_command(b"\t shutdown  \t");
        assert_eq!(name(&r), "shutdown");
        assert!(args(&r).is_empty());
    }

    #[test]
    fn case_sensitive() {
        assert!(matches!(resolve_command(b"FRAME"), Resolved::Unknown(_)));
        assert!(matches!(resolve_command(b"Frame"), Resolved::Unknown(_)));
    }

    #[test]
    fn unknown_returns_first_token() {
        let r = resolve_command(b"wat value");
        assert_eq!(name(&r), "wat");
    }

    #[test]
    fn empty_input_is_unknown_empty() {
        let r = resolve_command(b"");
        assert!(matches!(r, Resolved::Unknown(_)));
    }

    #[test]
    fn trace_command_resolves_with_arguments() {
        let r = resolve_command(b"trace irq off");
        assert_eq!(name(&r), "trace");
        assert_eq!(args(&r), b"irq off");
    }

    #[test]
    fn unload_command_resolves_with_arguments() {
        let r = resolve_command(b"unload kcomp_smoke");
        assert_eq!(name(&r), "unload");
        assert_eq!(args(&r), b"kcomp_smoke");
    }

    /// `trace` 是 Core 管理路径：类别开关真的落到运行时掩码上。
    #[test]
    #[cfg(feature = "trace")]
    fn trace_command_toggles_category_mask() {
        let _trace = crate::trace::test_support::GUARD.lock();
        crate::trace::reset_for_test();

        cmds::trace(b"irq off");
        let mask = crate::trace::enabled_mask() as u32;
        assert_eq!(mask & crate::trace::MASK_IRQ, 0, "irq 类别必须被关闭");
        assert_ne!(mask & crate::trace::MASK_TASK, 0, "其他类别不受影响");

        // 未知类别 / 缺动作不得改变掩码。
        cmds::trace(b"nope off");
        cmds::trace(b"irq");
        assert_eq!(crate::trace::enabled_mask() as u32, mask);

        cmds::trace(b"all on");
        assert_eq!(crate::trace::enabled_mask(), crate::trace::ENABLED_MASK_ALL);
        crate::trace::reset_for_test();
    }
}
