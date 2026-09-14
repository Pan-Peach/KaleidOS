//! Core Monitor 命令实现（machine / memory / help）。
//!
//! 全部只读：查询 core 状态并打印，不修改任何状态（无 god-mode）。
//! 输出走 `crate::print`（注入式，裸机 SBI / host 静默）。

use crate::component::load::ComponentLoadError;
use crate::machine::{IoSpace, MachineInfo};
use crate::memory;
use crate::printk;
use alloc::string::String;
use arch::{ResetImpl, ResetType, SystemReset};

/// 挂载 MachineInfo（core::init 完成时调用一次）：写入 machine 模块的唯一真相点。
pub fn mount(info: &MachineInfo) {
    crate::machine::commit(*info);
}

/// `help`：列出可用命令。
pub fn help(_line: &[u8]) {
    for cmd in crate::monitor::COMMANDS {
        printk!("  {:<12} - {}\n", cmd.name, cmd.help);
    }
}

/// `machine`：CPU / RAM 区域 / 设备清单。
pub fn machine(_line: &[u8]) {
    let Some(info) = crate::machine::committed() else {
        printk!("machine: not mounted\n");
        return;
    };
    printk!("boot hart: hart{}\n", info.boot_hart);
    printk!("cpus: {}\n", info.cpu_count);
    for i in 0..info.cpu_count {
        let c = &info.cpu_info[i];
        // raw()：CpuId 的 Display 是 "CPU0"；这里要裸 hart 号（hart0）。
        printk!("  hart{} boot={}\n", c.hart_id.raw(), c.boot_cpu);
    }
    printk!("memory regions: {}\n", info.mem_count);
    for i in 0..info.mem_count {
        let r = &info.memory_regions[i];
        printk!(
            "  [{:#x}, {:#x}) size={:#x}\n",
            r.base,
            r.base + r.size,
            r.size
        );
    }
    printk!("devices: {}\n", info.dev_count);
    for i in 0..info.dev_count {
        let d = &info.devices[i];
        let (space, base, size) = match d.space {
            IoSpace::Mmio { base, size } => ("mmio", base, size),
            IoSpace::Pio { base, size } => ("pio", base, size),
        };
        printk!("  {space} {base:#x}+{size:#x} irq={:?} compatible=[", d.irq);
        for j in 0..d.compat_count as usize {
            if j > 0 {
                printk!(", ");
            }
            printk!("{:?}", d.compatibles[j].as_str());
        }
        printk!("]\n");
    }
}

/// `memory`：物理内存分配器统计（free_block_counts 直方图）。
pub fn memory(_line: &[u8]) {
    let counts = memory::free_block_counts();
    // 只显示非空 order（4K 起：order 12 起）。
    let mut free_pages = 0usize;
    for (order, &n) in counts.iter().enumerate() {
        if n > 0 && order >= 12 {
            let block_pages = 1u64 << (order - 12);
            printk!(
                "free order{} ({:#x}): {} blocks\n",
                order,
                1usize << order,
                n
            );
            free_pages = free_pages.saturating_add((n as u64 * block_pages) as usize);
        }
    }
    printk!(
        "free pages: {} (~{:#x} bytes)\n",
        free_pages,
        free_pages * memory::ALLOC_GRANULE
    );
}

pub fn tasks(_line: &[u8]) {
    let table = crate::task::get_task_table().lock();
    printk!("tasks: {}\n", table.len());
    for (id, record) in table.iter() {
        printk!(
            "  id={} kstack={:#x} context={:?}\n",
            id,
            record.kstack.base,
            record.context
        );
    }
}

/// `load <name>`：走 Core 的组件加载语义入口（薄 caller，加载流程在
/// `component::load::load_and_start`，与组件 ABI `kcore_component_load` 同源）。
pub fn load(args: &[u8]) {
    let name = args.trim_ascii();
    if name.is_empty() {
        printk!("usage: load <name>\n");
        return;
    }
    let name = String::from_utf8_lossy(name);
    match crate::component::load::load_and_start(name.as_bytes()) {
        Ok(id) => {
            let entry = crate::component::registry::get_registry()
                .lock()
                .get(id)
                .map_or(0, |r| r.entry);
            printk!("load {}: OK (id={}, entry={:#x})\n", name, id.raw(), entry);
        }
        Err(ComponentLoadError::DeclareFailed) => {
            printk!("load {name}: already loaded\n");
        }
        Err(ComponentLoadError::NotFound) => {
            printk!("load {name}: no such component\n");
        }
        Err(error) => printk!("load {name}: {error:?}\n"),
    }
}

/// `components`：已加载组件列表。
pub fn components(_line: &[u8]) {
    let reg = crate::component::registry::get_registry().lock();
    printk!("components: {}\n", reg.len());
    for rec in reg.iter() {
        printk!(
            "  id={} state={:?} entry={:#x} base={:#x} name={}\n",
            rec.id.raw(),
            rec.state,
            rec.entry,
            rec.base,
            String::from_utf8_lossy(&rec.name)
        );
    }
}

/// `catalog`：列出仓库里**已找到**（可 `load`）的组件，并标注是否已加载。
///
/// 与 `components`（只列已加载）互补：这是"内核找到了哪些组件"的清单。
pub fn catalog(_line: &[u8]) {
    let Some(store) = crate::component::store::get_component_store() else {
        printk!("catalog: component store not mounted\n");
        return;
    };
    let entries = match store.list() {
        Ok(entries) => entries,
        Err(error) => {
            printk!("catalog: list failed: {error:?}\n");
            return;
        }
    };
    let reg = crate::component::registry::get_registry().lock();
    let mut count = 0usize;
    for entry in &entries {
        let Some(stem) = entry.name.strip_suffix(b".kcomp") else {
            continue;
        };
        let loaded = reg.iter().any(|record| record.name.as_slice() == stem);
        printk!(
            "  {} ({} bytes){}\n",
            String::from_utf8_lossy(stem),
            entry.len,
            if loaded { " [loaded]" } else { "" }
        );
        count += 1;
    }
    printk!("catalog: {count} available (load <name>)\n");
}

pub fn shutdown(_line: &[u8]) {
    ResetImpl::system_reset(ResetType::Shutdown);
}

pub fn reboot(_line: &[u8]) {
    ResetImpl::system_reset(ResetType::ColdReboot);
}
