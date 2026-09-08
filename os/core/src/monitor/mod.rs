//! Core Monitor —— 裸 Core 的交互调试面（只读为主，无组件依赖）。
//!
//! 设计约束（架构定案）：
//! - Monitor 是 Core 常态能力，不是"系统崩了才出现"的救火模式；
//! - 不依赖 Scheduler/Logger/Network 等任何 Component —— 组件全挂也要能跑；
//! - 只读观察系统状态（machine/memory），不提供 god-mode 修改；
//! - 不因拥有 console 而获得 authority（Oracle 审查结论）。
//!
//! 主循环：`core> _` 提示符 → 读行 → token 解析 → 命令表分发 → 循环。
//! 输入/输出都走 arch crate 的 Console backend，无注入层。

use crate::{print, printk};

mod cmds;

pub use cmds::mount;

const LINE_BUF: usize = 64;

/// 命令表：名称 + 执行函数（返回是否已执行；为未来多值参数预留 args）。
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
        help: "load a kcomp by name (e.g. load core_test)",
        run: cmds::load,
    },
    Command {
        name: "components",
        help: "list loaded components",
        run: cmds::components,
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
pub fn run() -> ! {
    printk!("KaleidOS Core Monitor\n");
    printk!("type 'help' for commands\n");

    let mut buf = [0u8; LINE_BUF];
    loop {
        printk!("core> ");
        let n = print::read_line(&mut buf);
        if n == 0 {
            continue;
        }
        let line = &buf[..n];
        printk!("\n");
        match resolve_command(line) {
            Resolved::Known(cmd, args) => (cmd.run)(args),
            Resolved::Unknown(first) => {
                printk!("unknown command '");
                print::print_bytes(first);
                printk!("' (try 'help')\n");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::string::ToString;

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
}
