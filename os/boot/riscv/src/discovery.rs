//! FDT 设备发现（RISC-V 共用，RV64/RV32 同一份）：设备描述 + 完整中断资源 +
//! PLIC 逻辑线绑定。
//!
//! - **全部空间窗口**：节点的**所有** supported `reg` 条目按固件顺序收进
//!   `spaces`（`spaces[0]` 是主窗口）；地址/长度按目标 `usize` 检查转换，
//!   不可表示的条目诊断后跳过（绝不截断成假地址）。
//! - **地址翻译**：只接受 identity 链——设备直接挂 root，或每一层 bus 祖先都
//!   声明空 `ranges`（devicetree spec：空 = 父子地址空间相同）。缺 `ranges`
//!   （子空间未映射到父空间）或非空 `ranges`（需要翻译）都**不**把子地址当 CPU
//!   地址：诊断并丢弃该设备。
//! - **全部 compatible**：不截断数量与长度。
//! - **完整中断资源**：`interrupts-extended` 每条 tuple 用其引用控制器的
//!   `#interrupt-cells` 切分（自带 phandle）；`interrupts` 用继承解析出的
//!   interrupt parent 的 `#interrupt-cells` 切分。保留完整 cells（GIC 的
//!   type/number/flags 等），不把解码值当逻辑 IRQ 号。
//! - **逻辑线 `line`**：只把**属于已配置 PLIC**、且 source 在该控制器声明
//!   范围内（`riscv,ndev`，缺省用后端支持的 source 上限）的 FDT specifier
//!   绑定成外部 IRQ 号；CPU-local（cpu-intc）与其它控制器一律 `line: None`。
//! - **发现错误**（boot 终止，不静默）：malformed tuple（长度不是 cells 的
//!   整数倍、截断）、unresolved controller phandle、控制器缺
//!   `#interrupt-cells` / phandle。
//!
//! 设备收集顺序 = `MachineInfo.devices` 顺序：root children 先、/soc children
//! 后（两层遍历，与旧行为一致）。

use alloc::boxed::Box;
use alloc::vec::Vec;
use fdt::nodes::{AsNode, Node};
use fdt::properties::reg::Reg;
use fdt::properties::values::StringList;
use fdt::properties::PHandle;
use kernel::machine::{DeviceDescriptor, InterruptResource, InterruptSpecifier, IoSpace};

/// boot 的 FDT parser flavour（与 main64 / main32 共用同一类型）。
pub type FdtParser<'a> = (
    fdt::parsing::unaligned::UnalignedParser<'a>,
    fdt::parsing::Panic,
);

/// PLIC 兼容串（真实 QEMU 的 `riscv,plic0` 与 fixture/新版的 `sifive,plic-1.0.0`）。
const PLIC_COMPATIBLES: &[&str] = &["riscv,plic0", "sifive,plic-1.0.0"];

/// 后端（`arch::riscv::plic`）支持的 source 数上限：`PLIC_SOURCE_COUNT` 的 boot
/// 侧镜像。`riscv,ndev` 缺省 / 非法时用它做绑定范围。
pub const PLIC_SOURCE_LIMIT: u32 = 1024;

/// 一条 specifier 允许的最大 cell 数（防御畸形 `#interrupt-cells`）。
const MAX_INTERRUPT_CELLS: usize = 16;

/// 沿 interrupt-parent 链向上解析的最大步数（防御自引用 / 畸形链）。
const MAX_PARENT_STEPS: usize = 16;

/// 设备是否声明了某个 PLIC compatible（单源：绑定与 `configure` 共用）。
pub fn is_plic_device(device: &DeviceDescriptor) -> bool {
    device
        .compatibles
        .iter()
        .any(|c| PLIC_COMPATIBLES.contains(&c.as_ref()))
}

/// 设备**主窗口**（`spaces[0]`，`kcore_device_claim` 返回的那一个）的 MMIO
/// 基址/长度；主窗口不是 MMIO / 设备没有窗口 → `None`。
pub fn primary_mmio(device: &DeviceDescriptor) -> Option<(usize, usize)> {
    match device.spaces.first() {
        Some(&IoSpace::Mmio { base, size }) => Some((base, size)),
        _ => None,
    }
}

/// 收集全部设备（root + /soc），并给属于已配置 PLIC 的中断资源绑定逻辑线。
pub fn collect_devices<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
) -> Result<Vec<DeviceDescriptor>, &'static str> {
    let plic = find_plic(tree);
    let mut devices: Vec<DeviceDescriptor> = Vec::new();
    collect(tree, tree.root().as_node().children(), &mut devices)?;
    if let Some(soc) = tree.find_node("/soc") {
        collect(tree, soc.children(), &mut devices)?;
    }
    if let Some(plic) = plic {
        bind_plic_lines(&mut devices, &plic);
    } else {
        kernel::log!("discovery", "no PLIC found; external IRQ unavailable");
    }
    Ok(devices)
}

/// 与设备收集**同序**地找 `configure_interrupt_controller` 会配置的那台 PLIC：
/// 第一个 PLIC-compatible 节点的 phandle + source 范围。
fn find_plic<'a>(tree: &fdt::Fdt<'a, FdtParser<'a>>) -> Option<PlicInfo> {
    if let Some(plic) = scan_plic(tree.root().as_node().children()) {
        return Some(plic);
    }
    let soc = tree.find_node("/soc")?;
    scan_plic(soc.children())
}

struct PlicInfo {
    phandle: u32,
    /// 最大合法外部 source（含）。
    max_source: u32,
}

fn scan_plic<'a>(children: impl IntoIterator<Item = Node<'a, FdtParser<'a>>>) -> Option<PlicInfo> {
    for node in children {
        if !node_matches(&node, PLIC_COMPATIBLES) {
            continue;
        }
        // 没有 phandle 的 PLIC 无法作为 interrupt-parent 被引用；保留
        // `configure` 路径（它只需要 reg），但不做 line 绑定。
        let phandle = node.property::<PHandle>().map(PHandle::as_u32)?;
        let ndev = node
            .properties()
            .find("riscv,ndev")
            .and_then(|property| property.as_value::<u32>().ok())
            .unwrap_or(0);
        return Some(PlicInfo {
            phandle,
            max_source: source_limit(ndev),
        });
    }
    None
}

/// `riscv,ndev` 声明的最大 source；缺省 / 非法时退回后端上限（source 0 是
/// "无中断"，因此上界还要排除 0）。
fn source_limit(ndev: u32) -> u32 {
    if ndev == 0 {
        PLIC_SOURCE_LIMIT - 1
    } else {
        ndev.min(PLIC_SOURCE_LIMIT - 1)
    }
}

/// 只把属于该 PLIC、cell 数正确（PLIC = 1 cell = source）、且 source 在
/// `1..=max_source` 内的资源绑定成逻辑外部 IRQ 号；其余保留 `line: None`。
fn bind_plic_lines(devices: &mut [DeviceDescriptor], plic: &PlicInfo) {
    for device in devices.iter_mut() {
        for resource in device.interrupts.iter_mut() {
            let InterruptSpecifier::Fdt { controller, cells } = &resource.specifier else {
                continue;
            };
            if *controller != plic.phandle || cells.len() != 1 {
                continue;
            }
            let source = cells[0];
            if (1..=plic.max_source).contains(&source) {
                resource.line = Some(source);
            }
        }
    }
}

fn collect<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    children: impl IntoIterator<Item = Node<'a, FdtParser<'a>>>,
    devices: &mut Vec<DeviceDescriptor>,
) -> Result<(), &'static str> {
    for child in children {
        if let Some(descriptor) = device_descriptor(tree, &child)? {
            devices.push(descriptor);
        }
    }
    Ok(())
}

/// 提取一个 FDT 节点的设备描述。
///
/// 过滤规则：必须有 compatible 且至少一个可表示的 `reg` 窗口（memory 无
/// compatible、cpus/chosen/pmu 无 reg，天然跳过）。`spaces` 收集**全部**
/// supported `reg` 条目（固件顺序，`spaces[0]` 是主窗口）；`compatibles`
/// 收集**全部**串（数量与长度都不截断）。
fn device_descriptor<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    child: &Node<'a, FdtParser<'a>>,
) -> Result<Option<DeviceDescriptor>, &'static str> {
    let mut compatibles: Vec<Box<str>> = Vec::new();
    if let Some(compatible) = child.properties().find("compatible") {
        if let Ok(list) = compatible.as_value::<StringList>() {
            for value in list {
                compatibles.push(Box::from(value));
            }
        }
    }
    if compatibles.is_empty() {
        return Ok(None);
    }

    let Some(reg) = child.reg() else {
        return Ok(None);
    };
    if !identity_addressable(child) {
        // 不能把需要翻译的子地址当 CPU 地址——诊断并丢弃这台设备。
        kernel::log!(
            "discovery",
            "device requires address translation; omitted (no MMIO address invented)"
        );
        return Ok(None);
    }
    let spaces = collect_spaces(reg);
    if spaces.is_empty() {
        return Ok(None);
    }

    Ok(Some(DeviceDescriptor {
        spaces: spaces.into_boxed_slice(),
        interrupts: interrupts_of(tree, child)?,
        compatibles: compatibles.into_boxed_slice(),
    }))
}

/// 收集 `reg` 的**全部** supported 条目（固件顺序）：u64 → `usize` 检查转换，
/// `base + size` 溢出检查；不可表示的条目诊断后跳过——绝不截断成假地址。
fn collect_spaces(reg: Reg<'_>) -> Vec<IoSpace> {
    let mut spaces = Vec::new();
    for entry in reg.iter::<u64, u64>() {
        let Ok(entry) = entry else {
            kernel::log!("discovery", "malformed reg entry; skipped");
            continue;
        };
        let (Ok(base), Ok(size)) = (usize::try_from(entry.address), usize::try_from(entry.len))
        else {
            kernel::log!(
                "discovery",
                "reg window {:#x}+{:#x} does not fit usize; skipped",
                entry.address,
                entry.len
            );
            continue;
        };
        if base.checked_add(size).is_none() {
            kernel::log!(
                "discovery",
                "reg window {base:#x}+{size:#x} overflows; skipped"
            );
            continue;
        }
        spaces.push(IoSpace::Mmio { base, size });
    }
    spaces
}

/// 设备是否位于 identity 可直达的地址链上：
/// - 直接挂在 root 下 → 固件的 CPU 地址空间，identity；
/// - 否则每一层祖先 bus 必须声明**空** `ranges`（devicetree spec：空 ranges =
///   父子地址空间相同）。缺 `ranges` = 子空间没有映射进父空间；非空 `ranges` =
///   需要地址翻译——本阶段都不支持。
fn identity_addressable<'a>(node: &Node<'a, FdtParser<'a>>) -> bool {
    let mut current = *node;
    while let Some(parent) = current.parent() {
        if parent.parent().is_none() {
            return true; // 直接挂在 root 下：子地址就是 CPU 地址
        }
        match parent.properties().find("ranges") {
            Some(property) if property.value.is_empty() => {}
            _ => return false,
        }
        current = parent;
    }
    false
}

/// 解析一个节点的完整中断资源列表（`interrupts-extended` 优先于 `interrupts`，
/// 与 devicetree spec / fdt crate 一致）；两者都缺失 → 空列表（合法）。
fn interrupts_of<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    node: &Node<'a, FdtParser<'a>>,
) -> Result<Box<[InterruptResource]>, &'static str> {
    if let Some(property) = node.properties().find("interrupts-extended") {
        return parse_extended(tree, property.value);
    }
    if let Some(property) = node.properties().find("interrupts") {
        return parse_legacy(tree, node, property.value);
    }
    Ok(Box::new([]))
}

/// `interrupts-extended`：每条 tuple = phandle + 该控制器 `#interrupt-cells`
/// 个 cell。截断 / unresolved phandle → 发现错误。
fn parse_extended<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    mut rest: &[u8],
) -> Result<Box<[InterruptResource]>, &'static str> {
    let mut resources = Vec::new();
    while !rest.is_empty() {
        let Some((phandle_bytes, tail)) = rest.split_at_checked(4) else {
            return Err("interrupts-extended: truncated phandle");
        };
        let phandle = u32::from_be_bytes(phandle_bytes.try_into().expect("4 bytes"));
        rest = tail;
        let controller = resolve_controller(tree, phandle)?;
        let cells_count = interrupt_cells(&controller)?;
        let bytes = cells_count * 4;
        let Some((cells_bytes, tail)) = rest.split_at_checked(bytes) else {
            return Err("interrupts-extended: truncated specifier");
        };
        rest = tail;
        resources.push(InterruptResource {
            specifier: InterruptSpecifier::Fdt {
                controller: phandle,
                cells: collect_cells(cells_bytes),
            },
            line: None,
        });
    }
    Ok(resources.into_boxed_slice())
}

/// `interrupts`：全部 tuple 共享同一个（继承解析出的）interrupt parent；
/// 长度必须是 `#interrupt-cells` 的整数倍。
fn parse_legacy<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    node: &Node<'a, FdtParser<'a>>,
    bytes: &[u8],
) -> Result<Box<[InterruptResource]>, &'static str> {
    if bytes.is_empty() {
        return Ok(Box::new([]));
    }
    let controller = interrupt_parent(tree, node)?;
    let cells_count = interrupt_cells(&controller)?;
    let stride = cells_count * 4;
    if !bytes.len().is_multiple_of(stride) {
        return Err("interrupts: length is not a multiple of #interrupt-cells");
    }
    let phandle = controller
        .property::<PHandle>()
        .map(PHandle::as_u32)
        .ok_or("interrupt controller node has no phandle")?;
    let mut resources = Vec::new();
    for cells_bytes in bytes.chunks_exact(stride) {
        resources.push(InterruptResource {
            specifier: InterruptSpecifier::Fdt {
                controller: phandle,
                cells: collect_cells(cells_bytes),
            },
            line: None,
        });
    }
    Ok(resources.into_boxed_slice())
}

fn collect_cells(bytes: &[u8]) -> Box<[u32]> {
    bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_be_bytes(chunk.try_into().expect("chunk is 4 bytes")))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// 解析 interrupt parent：Linux `of_irq_find_parent` 的规则——先跟自己的
/// `interrupt-parent`；没有就上溯 devicetree parent；停在第一个有
/// `#interrupt-cells` 的节点。unresolved phandle / 链过深 → 发现错误。
fn interrupt_parent<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    node: &Node<'a, FdtParser<'a>>,
) -> Result<Node<'a, FdtParser<'a>>, &'static str> {
    let mut current = *node;
    for _ in 0..MAX_PARENT_STEPS {
        let next = if let Some(property) = current.properties().find("interrupt-parent") {
            let phandle = property
                .as_value::<u32>()
                .map_err(|_| "invalid interrupt-parent")?;
            resolve_controller(tree, phandle)?
        } else {
            current.parent().ok_or("device has no interrupt parent")?
        };
        if next.properties().find("#interrupt-cells").is_some() {
            return Ok(next);
        }
        current = next;
    }
    Err("interrupt-parent chain is too deep")
}

fn resolve_controller<'a>(
    tree: &fdt::Fdt<'a, FdtParser<'a>>,
    phandle: u32,
) -> Result<Node<'a, FdtParser<'a>>, &'static str> {
    tree.root()
        .resolve_phandle(PHandle::new(phandle))
        .ok_or("interrupt controller phandle does not resolve")
}

fn interrupt_cells<'a>(controller: &Node<'a, FdtParser<'a>>) -> Result<usize, &'static str> {
    let property = controller
        .properties()
        .find("#interrupt-cells")
        .ok_or("interrupt controller has no #interrupt-cells")?;
    let count = property
        .as_value::<u32>()
        .map_err(|_| "invalid #interrupt-cells")? as usize;
    if count == 0 || count > MAX_INTERRUPT_CELLS {
        return Err("#interrupt-cells out of range");
    }
    Ok(count)
}

/// 节点 compatible（任一命中即真）；用于 PLIC 扫描。
fn node_matches<'a>(node: &Node<'a, FdtParser<'a>>, compatibles: &[&str]) -> bool {
    let Some(property) = node.properties().find("compatible") else {
        return false;
    };
    let Ok(list) = property.as_value::<StringList>() else {
        return false;
    };
    list.into_iter().any(|value| compatibles.contains(&value))
}
