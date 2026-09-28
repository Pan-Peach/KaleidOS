//! Core Monitor 命令实现（machine / memory / help / trace …）。
//!
//! 查询命令只读：查询 core 状态并打印，不修改任何状态（无 god-mode）。
//! 管理命令（load / trace / shutdown…）走 Core 已有的语义入口；trace 掩码是
//! Core 管理路径的一部分（组件没有全局 trace-control authority，只能读）。
//! 输出走 `crate::print`（注入式，裸机 SBI / host 静默）。

use crate::component::endpoint::ExecutionDomain;
use crate::component::load::ComponentLoadError;
use crate::machine::{IoSpace, MachineInfo};
use crate::memory;
use crate::printk;
use alloc::string::String;
use alloc::vec::Vec;
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
    printk!("boot hart: hart{}\n", info.boot_hardware_id.raw());
    printk!("cpus: {}\n", info.cpu_count);
    for i in 0..info.cpu_count {
        let c = &info.cpu_info[i];
        // raw()：HardwareCpuId 的 Display 是 "hwcpuN"；这里要裸 hart 号（hartN）。
        printk!("  hart{} boot={}\n", c.hardware_id.raw(), c.boot_cpu);
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

/// 解析 Monitor 的部署域 token（`load <name> [kind]`）。
///
/// 只做**词法**映射：部署是否真的可执行由 Core 的创建入口验证（`create_component`
/// 对未实现域显式拒绝）——Monitor 不是 authority，不能凭输入授予部署。
fn parse_domain(token: &[u8]) -> Option<ExecutionDomain> {
    match token {
        b"native" => Some(ExecutionDomain::KernelNative),
        b"isolated" => Some(ExecutionDomain::IsolatedNative),
        b"sandboxed" => Some(ExecutionDomain::SandboxedNative),
        _ => None,
    }
}

/// `load <name> [kind]`：走 Core 的组件实例创建语义入口（薄 caller，创建流程在
/// `component::load::load_and_start`，与组件 ABI `kcore_component_load` 同源）。
///
/// `kind` 是**部署请求**（省略 = `native`）：Core 验证后提交，未实现域显式拒绝。
pub fn load(args: &[u8]) {
    let mut tokens = args
        .trim_ascii()
        .split(|&b| b.is_ascii_whitespace())
        .filter(|token| !token.is_empty());
    let Some(name) = tokens.next() else {
        printk!("usage: load <name> [native|isolated|sandboxed]\n");
        return;
    };
    let kind = match tokens.next() {
        None => ExecutionDomain::KernelNative,
        Some(token) => match parse_domain(token) {
            Some(kind) => kind,
            None => {
                printk!("load: unknown deployment '");
                crate::print::print_bytes(token);
                printk!("' (native|isolated|sandboxed)\n");
                return;
            }
        },
    };
    if tokens.next().is_some() {
        printk!("usage: load <name> [native|isolated|sandboxed]\n");
        return;
    }
    let name = String::from_utf8_lossy(name);
    // 每次 `load` 都创建**全新组件**（独立 writable image）；同名再次 load 不再
    // 短路——多实例是合法语义（组件 ABI 的 `kcore_component_create` 同理）。
    match crate::component::load::load_and_start(name.as_bytes(), kind) {
        Ok(id) => {
            let create = {
                let reg = crate::component::registry::get_registry().lock();
                let Some(record) = reg.get(id) else {
                    printk!("load {name}: component vanished\n");
                    return;
                };
                record.loaded.create
            };
            printk!(
                "load {}: OK (id={}, create={:#x})\n",
                name,
                id.raw(),
                create
            );
            // 组合动作：该实例若发布了调度策略 endpoint，**显式**发现 + 选择它
            // （Core 的调度路径不按名字发现；选择只提交 EndpointId）。不是策略
            // provider 的组件自然没有这个端口，静默跳过。
            match crate::sched::select_provider(id) {
                Ok(()) => printk!("load {name}: scheduler policy selected (id={})\n", id.raw()),
                Err(crate::sched::SchedError::PolicyEndpoint(
                    crate::component::endpoint::EndpointError::EndpointNotFound,
                )) => {}
                Err(error) => {
                    printk!("load {name}: scheduler selection failed: {error:?}\n");
                }
            }
            // 组件可能在 `kcomp_instance_create` 期间创建了任务（例如
            // driver_prober 的 post-init dispatch 任务）。create 期间 publish 是
            // staged：消费者必须等 provider `Ready`，所以这类任务只能在 create
            // 提交之后运行。Monitor 是交互态下唯一的调度锚点，这里在加载成功后把
            // CPU 交给调度器；没有 Runnable 任务时 `sched::run()` 是 no-op。
            if let Err(error) = crate::sched::run() {
                printk!("load {name}: post-load scheduling failed: {error:?}\n");
            }
        }
        Err(ComponentLoadError::NotFound) => {
            printk!("load {name}: no such component\n");
        }
        Err(error) => printk!("load {name}: {error:?}\n"),
    }
}

/// `unload <name>`：优雅停止该 artifact 的实例（`Ready → Stopping → Stopped`）。
///
/// 薄 caller：停止编排在 `component/exit.rs::stop_component`（拒绝拥有未退出
/// 任务的组件；调用 `kcomp_instance_destroy`；Core 兜底回收）。同名 artifact 可以
/// 有多个组件，本命令停掉该 name 的**全部**组件。记录保留——不回收 backing、
/// 不退役组件，`components` 仍能看到 `state=Stopped`。
pub fn unload(args: &[u8]) {
    let name = args.trim_ascii();
    if name.is_empty() {
        printk!("usage: unload <name>\n");
        return;
    }
    let name = String::from_utf8_lossy(name);
    let ids: Vec<crate::component::ComponentId> = {
        let reg = crate::component::registry::get_registry().lock();
        reg.iter()
            .filter(|record| record.name.as_slice() == name.as_bytes())
            .map(|record| record.id)
            .collect()
    };
    if ids.is_empty() {
        printk!("unload {name}: no such component\n");
        return;
    }
    for id in ids {
        match crate::component::exit::stop_component(id) {
            Ok(()) => printk!("unload {}: OK (id={}, state=Stopped)\n", name, id.raw()),
            Err(error) => printk!("unload {}: id={} {error:?}\n", name, id.raw()),
        }
    }
}

/// `components`：已声明组件列表（id + name + loaded image 投影）。
pub fn components(_line: &[u8]) {
    let reg = crate::component::registry::get_registry().lock();
    printk!("components: {}\n", reg.len());
    for rec in reg.iter() {
        printk!(
            "  id={} state={:?} name={} create={:#x} base={:#x}\n",
            rec.id.raw(),
            rec.state,
            String::from_utf8_lossy(&rec.name),
            rec.loaded.create,
            rec.loaded.base
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

/// Monitor 的事件类别：名字 → 使能位并集（位定义见 `trace::ring`）。
struct TraceCategory {
    name: &'static str,
    mask: u32,
}

const TRACE_CATEGORIES: &[TraceCategory] = &[
    TraceCategory {
        name: "task",
        mask: crate::trace::MASK_TASK,
    },
    TraceCategory {
        name: "policy",
        mask: crate::trace::MASK_POLICY,
    },
    TraceCategory {
        name: "component",
        mask: crate::trace::MASK_COMPONENT,
    },
    TraceCategory {
        name: "resource",
        mask: crate::trace::MASK_RESOURCE,
    },
    TraceCategory {
        name: "endpoint",
        mask: crate::trace::MASK_ENDPOINT,
    },
    TraceCategory {
        name: "irq",
        mask: crate::trace::MASK_IRQ,
    },
];

/// `trace [<category|all> on|off]`：查看 / 开关运行时事件使能掩码。
///
/// 掩码是 Core 管理路径的一部分：组件没有全局 trace-control authority，
/// 只能经 `kcore_trace_stats` **读**。被关闭的事件不记录、不消耗 `seq`。
pub fn trace(line: &[u8]) {
    if !cfg!(feature = "trace") {
        printk!("trace: not compiled in (CONFIG_TRACE=n)\n");
        return;
    }
    if line.trim_ascii().is_empty() {
        trace_status();
        return;
    }
    let mut tokens = line.trim_ascii().split(|&b| b.is_ascii_whitespace());
    let name = tokens.next().unwrap_or(b"");
    let mask = if name == b"all" {
        crate::trace::ENABLED_MASK_ALL as u32
    } else if let Some(category) = TRACE_CATEGORIES.iter().find(|c| c.name.as_bytes() == name) {
        category.mask
    } else {
        printk!("trace: unknown category (try 'trace')\n");
        return;
    };
    let current = crate::trace::enabled_mask() as u32;
    let next = match tokens.next() {
        Some(b"on") => current | mask,
        Some(b"off") => current & !mask,
        _ => {
            printk!("usage: trace <all|task|policy|component|resource|endpoint|irq> on|off\n");
            return;
        }
    };
    crate::trace::set_enabled_mask(next);
    trace_status();
}

/// `trace` 无参数：打印采集状态 + 各事件类别的 on/off。
fn trace_status() {
    let stats = crate::trace::stats();
    printk!(
        "trace: capacity={} next_seq={} oldest_seq={} overwritten={} mask={:#06x}\n",
        crate::trace::capacity(),
        stats.next_seq,
        stats.oldest_seq,
        stats.overwritten_total,
        stats.enabled_mask
    );
    let mask = crate::trace::enabled_mask() as u32;
    for category in TRACE_CATEGORIES {
        let state = if mask & category.mask == 0 {
            "off"
        } else {
            "on"
        };
        printk!("  {:<10} {}\n", category.name, state);
    }
}

pub fn shutdown(_line: &[u8]) {
    ResetImpl::system_reset(ResetType::Shutdown);
}

pub fn reboot(_line: &[u8]) {
    ResetImpl::system_reset(ResetType::ColdReboot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_domain_maps_each_token_to_its_execution_domain() {
        assert_eq!(parse_domain(b"native"), Some(ExecutionDomain::KernelNative));
        assert_eq!(
            parse_domain(b"isolated"),
            Some(ExecutionDomain::IsolatedNative)
        );
        assert_eq!(
            parse_domain(b"sandboxed"),
            Some(ExecutionDomain::SandboxedNative)
        );
    }

    #[test]
    fn parse_domain_rejects_unknown_and_case_variants() {
        assert_eq!(parse_domain(b""), None);
        assert_eq!(parse_domain(b"kernel-native"), None);
        assert_eq!(parse_domain(b"Native"), None);
        assert_eq!(parse_domain(b"bogus"), None);
        // `wasm` 是执行模型 / runtime 维度，**不是**执行域：不接受为 kind
        // （见 `endpoint.rs::ExecutionDomain` 文档）。
        assert_eq!(parse_domain(b"wasm"), None);
    }
}
