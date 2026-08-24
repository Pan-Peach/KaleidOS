//! Core Monitor —— 裸 Core 的交互调试面（只读为主，无组件依赖）。
//!
//! 设计约束（架构定案）：
//! - Monitor 是 Core 常态能力，不是"系统崩了才出现"的救火模式；
//! - 不依赖 Scheduler/Logger/Network 等任何 Component —— 组件全挂也要能跑；
//! - 只读观察系统状态（machine/memory/frame），不提供 god-mode 修改；
//! - 不因拥有 console 而获得 authority（Oracle 审查结论）。
//!
//! 主循环：`core> _` 提示符 → 读行 → token 解析 → 命令表分发 → 循环。
//! 输入走注入（print::install_reader），输出走注入（print::install）。

use crate::print;

mod cmds;

pub use cmds::mount;

const LINE_BUF: usize = 64;

/// 命令表：名称 + 执行函数（返回是否已执行；为未来多值参数预留 args）。
struct Command {
    name: &'static str,
    run: fn(line: &[u8]),
}

const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        run: cmds::help,
    },
    Command {
        name: "machine",
        run: cmds::machine,
    },
    Command {
        name: "memory",
        run: cmds::memory,
    },
    Command {
        name: "frame",
        run: cmds::frame,
    },
];

/// 进入 Monitor 主循环（永不返回）。
pub fn run() -> ! {
    print::print(format_args!("KaleidOS Core Monitor\n"));
    print::print(format_args!("type 'help' for commands\n"));

    let mut buf = [0u8; LINE_BUF];
    loop {
        print::print(format_args!("core> "));
        let n = print::read_line(&mut buf);
        if n == 0 {
            continue;
        }
        let line = &buf[..n];
        let first = line.split(|&b| b == b' ').next().unwrap_or(b"");
        // 匹配命令名（大小写敏感，精确）
        let cmd = COMMANDS.iter().find(|c| c.name.as_bytes() == first);
        print::print(format_args!("\n"));
        match cmd {
            Some(c) => (c.run)(line),
            None => {
                print::print(format_args!("unknown command '"));
                print::print_bytes(first);
                print::print(format_args!("' (try 'help')\n"));
            }
        }
    }
}
