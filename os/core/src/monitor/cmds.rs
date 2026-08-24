//! Core Monitor 命令实现（machine / memory / frame / help）。
//!
//! 全部只读：查询 core 状态并打印，不修改任何状态（无 god-mode）。
//! 输出走 `crate::print`（注入式，裸机 SBI / host 静默）。

use crate::printk;
use crate::machine::MachineInfo;
use crate::memory;
use arch::{Arch, ResetType};
use spin::Mutex;

/// Monitor 持有的 MachineInfo 副本（core::init 时 mount）。
static MACHINE: Mutex<Option<MachineInfo>> = Mutex::new(None);

/// 挂载 MachineInfo（core::init 完成时调用一次）。
pub fn mount(info: &MachineInfo) {
    *MACHINE.lock() = Some(*info);
}

/// `help`：列出可用命令。
pub fn help(_line: &[u8]) {
    for cmd in crate::monitor::COMMANDS {
        printk!("  {:<12} - {}\n", cmd.name, cmd.help);
    }
}

/// `machine`：CPU / RAM 区域 / 设备清单。
pub fn machine(_line: &[u8]) {
    let guard = MACHINE.lock();
    let Some(info) = guard.as_ref() else {
        printk!("machine: not mounted\n");
        return;
    };
    printk!("boot hart: hart{}\n", info.boot_hart);
    printk!("cpus: {}\n", info.cpu_count);
    for i in 0..info.cpu_count {
        let c = &info.cpu_info[i];
        printk!("  hart{} boot={}\n", c.hart_id, c.boot_cpu);
    }
    printk!("memory regions: {}\n", info.mem_count);
    for i in 0..info.mem_count {
        let r = &info.memory_regions[i];
        printk!("  [{:#x}, {:#x}) size={:#x}\n", r.base, r.base + r.size, r.size);
    }
    printk!("devices: {}\n", info.dev_count);
    for i in 0..info.dev_count {
        let d = &info.devices[i];
        printk!("  mmio {:#x}+{:#x} irq={:?}\n", d.mmio_base, d.mmio_size, d.irq);
    }
}

/// `memory`：帧分配器统计（free_block_counts 直方图）。
pub fn memory(_line: &[u8]) {
    let counts = memory::free_block_counts();
    // 只显示非空 order（4K 起：order 12 起）
    let mut free_frames = 0usize;
    for (order, &n) in counts.iter().enumerate() {
        if n > 0 && order >= 12 {
            let block_frames = 1u64 << (order - 12); // order 12=1 frame
            printk!("free order{} ({:#x}): {} blocks\n", order, 1usize << order, n);
            free_frames = free_frames.saturating_add((n as u64 * block_frames) as usize);
        }
    }
    printk!("free frames: {} (~{:#x} bytes)\n", free_frames, free_frames * memory::FRAME_SIZE);
}

/// `frame <addr>`：按物理地址（hex）显示帧归属 —— 现在只显示"帧号对应范围"，
/// per-frame 归属查询待帧真相 API（M1/M2 真相存储落地）。
pub fn frame(line: &[u8]) {
    // 解析 addr 参数（hex）
    let mut addr_str = "";
    for tok in line.split(|&b| b == b' ') {
        if !tok.is_empty() {
            addr_str = core::str::from_utf8(tok).unwrap_or("");
            break;
        }
    }
    if addr_str.is_empty() {
        printk!("usage: frame <addr>\n");
        return;
    }
    let addr = usize::from_str_radix(addr_str.trim_start_matches("0x"), 16);
    match addr {
        Ok(pa) => {
            let frame = memory::FrameId::from_pa(pa);
            printk!("pa {:#x} -> frame {} (start {:#x})\n", pa, frame.raw(), frame.start_pa());
        }
        Err(_) => printk!("invalid address: '{}'\n", addr_str),
    }
}

pub fn shutdown(_line: &[u8]) {
    arch::ArchImpl::system_reset(ResetType::Shutdown);
}

pub fn reboot(_line: &[u8]) {
    arch::ArchImpl::system_reset(ResetType::ColdReboot);
}
