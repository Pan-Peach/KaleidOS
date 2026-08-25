//! Core Monitor —— 裸 Core 的交互调试面（只读为主，无组件依赖）。
//!
//! 设计约束（架构定案）：
//! - Monitor 是 Core 常态能力，不是"系统崩了才出现"的救火模式；
//! - 不依赖 Scheduler/Logger/Network 等任何 Component —— 组件全挂也要能跑；
//! - 只读观察系统状态（machine/memory/frame），不提供 god-mode 修改；
//! - 不因拥有 console 而获得 authority（Oracle 审查结论）。
//!
//! 主循环：`core> _` 提示符 → 读行 → token 解析 → 命令表分发 → 循环。
//! 输入/输出都走 arch（read_line / console_getc / console_write_byte），无注入层。

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
        help: "frame allocator stats",
        run: cmds::memory,
    },
    Command {
        name: "frame",
        help: "query frame by physical address (hex)",
        run: cmds::frame,
    },
    Command {
        name: "tasks",
        help: "list tasks",
        run: cmds::tasks,
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
        let first = line.split(|&b| b == b' ').next().unwrap_or(b"");
        // 匹配命令名（大小写敏感，精确）
        let cmd = COMMANDS.iter().find(|c| c.name.as_bytes() == first);
        printk!("\n");
        match cmd {
            Some(c) => (c.run)(line),
            None => {
                printk!("unknown command '");
                print::print_bytes(first);
                printk!("' (try 'help')\n");
            }
        }
    }
}
