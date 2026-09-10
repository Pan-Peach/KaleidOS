use crate::vm::runtime::RuntimeVm;
use crate::vm::{bootstrap, layout};
use arch::CpuArch;
use core::arch::global_asm;
use core::panic::PanicInfo;
use fdt::nodes::AsNode;
use fdt::properties::values::StringList;
use kernel::machine::{
    CompatStr, CpuId, CpuInfo, DeviceDescriptor, IoSpace, MachineInfo, MemoryRegion,
};

#[path = "console.rs"]
mod console;

global_asm!(include_str!("entry64.S"));

// 链接脚本符号：本文档镜像（bootstrap + core 单一 kaleidos.elf）的物理范围。
// 取地址（不是值）：这段是 Core 自己，启动后永久 Reserved。
// __initpkg_*：内嵌组件归档（init.kpkg = cpio）所在段（见下方 INITPKG 注入）。
unsafe extern "C" {
    static __bootstrap_start: u8;
    static __bootstrap_end: u8;
    static __text_vma_start: u8;
    static __text_vma_end: u8;
    static __rodata_vma_start: u8;
    static __rodata_vma_end: u8;
    static __initpkg_start: u8;
    static __initpkg_end: u8;
    static __data_vma_start: u8;
    static __data_vma_end: u8;
    static __bss_vma_start: u8;
    static __bss_vma_end: u8;
    static high_boot_stack_top: u8;
}

/// Data that must survive the low-to-high-half control-flow hand-off.
///
/// The object remains on the original discovery stack while
/// `enter_high_half` switches to a separate high-half stack.
#[repr(C)]
struct BootContext {
    info: MachineInfo,
    reserved: [MemoryRegion; 1],
}

fn linker_addr(symbol: *const u8) -> usize {
    symbol as usize
}

fn print_linker_layout() {
    let image_start =
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__bootstrap_start)));
    let text_start_vma = linker_addr(core::ptr::addr_of!(__text_vma_start));
    let low_end = bootstrap::physical_address_of(text_start_vma);
    kernel::log!(
        "layout",
        "early bootstrap VMA/LMA: {:#x}-{:#x}",
        image_start,
        low_end
    );
    kernel::log!(
        "layout",
        ".text VMA {:#x}-{:#x}, LMA {:#x}-{:#x}",
        text_start_vma,
        linker_addr(core::ptr::addr_of!(__text_vma_end)),
        bootstrap::physical_address_of(text_start_vma),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__text_vma_end),))
    );
    kernel::log!(
        "layout",
        ".rodata VMA {:#x}-{:#x}, LMA {:#x}-{:#x}",
        linker_addr(core::ptr::addr_of!(__rodata_vma_start)),
        linker_addr(core::ptr::addr_of!(__rodata_vma_end)),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__rodata_vma_start),)),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__rodata_vma_end),))
    );
    kernel::log!(
        "layout",
        ".initpkg VMA {:#x}-{:#x}, LMA {:#x}-{:#x}",
        linker_addr(core::ptr::addr_of!(__initpkg_start)),
        linker_addr(core::ptr::addr_of!(__initpkg_end)),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__initpkg_start),)),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__initpkg_end),))
    );
    kernel::log!(
        "layout",
        ".data VMA {:#x}-{:#x}, LMA {:#x}-{:#x}",
        linker_addr(core::ptr::addr_of!(__data_vma_start)),
        linker_addr(core::ptr::addr_of!(__data_vma_end)),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__data_vma_start),)),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__data_vma_end),))
    );
    kernel::log!(
        "layout",
        ".bss VMA {:#x}-{:#x}, LMA {:#x}-{:#x}",
        linker_addr(core::ptr::addr_of!(__bss_vma_start)),
        linker_addr(core::ptr::addr_of!(__bss_vma_end)),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__bss_vma_start),)),
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__bss_vma_end),))
    );
    kernel::log!(
        "layout",
        "kernel VMA {:#x}-{:#x}, image LMA {:#x}-{:#x}",
        linker_addr(core::ptr::addr_of!(__bootstrap_start)),
        linker_addr(core::ptr::addr_of!(__bootstrap_end)),
        image_start,
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__bootstrap_end),))
    );
}

// 内嵌组件归档：编译期把 init.kpkg（make init.kpkg 生成）注入 .initpkg 段。
// rustc 原生 link_section → ABI 与镜像一致，无需 objcopy。
#[used]
#[unsafe(link_section = ".initpkg")]
static INITPKG: [u8; include_bytes!("../../../../tools/qemu/init.kpkg").len()] =
    *include_bytes!("../../../../tools/qemu/init.kpkg");

/// OpenSBI 选定的 boot hart 进入 payload；其它 hart 留在 firmware 的 warm-boot 路径。
#[unsafe(no_mangle)]
extern "C" fn bootstrap_main(hart_id: usize, dtb_pa: usize, kernel_pa: usize) -> ! {
    arch::CpuImpl::init();
    kernel::log!("bootstrap", "arch init OK");
    kernel::log!("bootstrap", "KaleidOS bootstrap");
    kernel::log!("bootstrap", "========================================");

    // FDT 发现：直接吃 OpenSBI 给的 dtb 物理地址（unsafe：该地址有效性 Rust 无从验证）
    match unsafe { fdt::Fdt::from_ptr_unaligned(dtb_pa as *const u8) } {
        Ok(tree) => {
            kernel::log!("bootstrap", "FDT magic: OK");

            // 归一化：fdt 类型 → core::machine 类型（owned，DTB 用完可丢）。
            // MachineInfo 是定长数组 + count（无借用），字符串用 CompatStr 内嵌复制。
            let mut memory_regions = [MemoryRegion { base: 0, size: 0 }; 16];
            let mut cpu_info = [CpuInfo {
                boot_cpu: false,
                hart_id: CpuId(0),
            }; 8];
            let mut devices = [DeviceDescriptor::empty(); 26];

            let mut mem_count = 0usize;
            for region in tree.root().memory().reg().iter::<u64, u64>() {
                let Ok(r) = region else { continue };
                if mem_count >= memory_regions.len() {
                    kernel::log!("discovery", "too many RAM regions; dropping");
                    continue;
                }
                memory_regions[mem_count] = MemoryRegion {
                    base: r.address as usize,
                    size: r.len as usize,
                };
                mem_count += 1;
            }

            let mut cpu_count = 0usize;
            for cpu in tree.root().cpus().iter() {
                if cpu_count >= cpu_info.len() {
                    kernel::log!("discovery", "too many CPUs; dropping");
                    continue;
                }
                let hart = cpu.reg::<u64>().first().unwrap_or(0);
                cpu_info[cpu_count] = CpuInfo {
                    boot_cpu: hart == hart_id as u64,
                    hart_id: CpuId(hart as usize),
                };
                cpu_count += 1;
            }

            // 两层遍历：root 挂系统级设备（QEMU 把 fw-cfg/flash 放在 /），/soc 挂总线设备
            let mut dev_count = 0usize;
            collect_devices(
                tree.root().as_node().children(),
                &mut devices,
                &mut dev_count,
            );
            if let Some(soc) = tree.find_node("/soc") {
                collect_devices(soc.children(), &mut devices, &mut dev_count);
            }

            let info = MachineInfo {
                boot_hart: hart_id,
                cpu_count,
                cpu_info,
                mem_count,
                memory_regions,
                dev_count,
                devices,
            };

            if mem_count == 0 {
                panic!("Sv39 early map failed: no RAM region");
            }

            let linked_image_start =
                bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__bootstrap_start)));
            let linked_image_end =
                bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__bootstrap_end)));
            let image_size = linked_image_end
                .checked_sub(linked_image_start)
                .expect("invalid linked kernel image range");

            // Pick the RAM region that actually contains the loaded kernel
            // image instead of assuming it is the first one.  This keeps the
            // boot mapping correct on platforms with multiple RAM regions.
            let ram = memory_regions[..mem_count]
                .iter()
                .find(|r| {
                    r.base <= kernel_pa
                        && kernel_pa
                            .checked_add(image_size)
                            .is_some_and(|end| end <= r.base + r.size)
                })
                .copied()
                .unwrap_or(memory_regions[0]);

            // Section-aware high-half alias: map each linker section with the
            // permission it actually needs.  Sv39 enforces W^X / no-write on
            // the formal image once the high-half alias is active.
            // 段范围与权限的唯一来源：vm::layout（boot/runtime 共同输入）。
            let layout = layout::kernel_layout();

            match unsafe {
                bootstrap::init(
                    kernel_pa,
                    linked_image_start,
                    image_size,
                    ram.base,
                    ram.size,
                    &layout.sections(),
                )
            } {
                Ok(()) => {}
                Err(error) => panic!("Sv39 early map failed: {:?}", error),
            }

            // 本文档镜像范围 → reserved（Core 自己，永久保留）
            let image_start = kernel_pa;
            let context = BootContext {
                info,
                reserved: [MemoryRegion {
                    base: image_start,
                    size: image_size,
                }],
            };

            // Keep the context pointer in the original stack while the
            // hand-off switches to a separate high-half stack.  FDT discovery
            // and the early root remain low-address work; Core starts only
            // after the high-half hand-off.
            let context_ptr = &context as *const BootContext as usize;
            kernel::log!("bootstrap", "Sv39 dual map OK; entering high-half");
            unsafe {
                arch::riscv::mmu::activate(bootstrap::root_pa() >> 12, 0);
                bootstrap::enter_high_half(
                    bootstrap_high as *const () as usize,
                    context_ptr,
                    core::ptr::addr_of!(high_boot_stack_top) as usize,
                );
            }
        }
        Err(_) => {
            panic!("FDT magic: BAD");
        }
    }
}

/// First Rust entry reached through the high-half alias.
///
/// The identity mapping is still present only as a bootstrap safety alias;
/// this function and the rest of the kernel are already linked in the high
/// VMA, with their bytes loaded at the low physical LMA.
#[unsafe(no_mangle)]
extern "C" fn bootstrap_high(context_ptr: usize) -> ! {
    arch::CpuImpl::init();

    let context = unsafe { &*(context_ptr as *const BootContext) };
    kernel::log!("bootstrap", "entered high-half kernel");
    print_linker_layout();
    kernel::log!("bootstrap", "MachineInfo dump:");
    kernel::printk!("{:#?}\n", context.info);
    kernel::log!("bootstrap", "BOOT DISCOVERY OK");

    #[cfg(feature = "selftest")]
    {
        crate::selftest::run();
    }

    #[cfg(not(feature = "selftest"))]
    {
        kernel::log!("core", "core init: ");

        match kernel::init(&context.info, &context.reserved) {
            Ok(()) => {
                kernel::log!("core", "BOOT CORE OK");

                // 构造 runtime VM（Sv39 动态根）并启用：buddy 已活
                // （kernel::init 之后），替换 bootstrap 临时 root。
                // kernel_pa = 镜像加载地址（context.reserved[0] 即镜像 PA 范围）。
                let runtime_layout = layout::kernel_layout();
                let kernel_pa = context.reserved[0].base;
                let runtime_vm = RuntimeVm::build(&runtime_layout, kernel_pa, &context.info)
                    .expect("Sv39 runtime Vm build failed");
                runtime_vm
                    .verify(&runtime_layout, kernel_pa)
                    .expect("Sv39 runtime Vm verify failed");
                runtime_vm
                    .activate()
                    .expect("Sv39 runtime Vm activate failed");
                // 内嵌组件仓库：.initpkg section = init.kpkg（cpio 归档）
                let pkg_start = core::ptr::addr_of!(__initpkg_start) as usize;
                let pkg_end = core::ptr::addr_of!(__initpkg_end) as usize;
                let pkg = unsafe {
                    core::slice::from_raw_parts(pkg_start as *const u8, pkg_end - pkg_start)
                };
                kernel::component::store::init(pkg);
                if let Some(store) = kernel::component::store::get_component_store() {
                    let list = store.list();
                    match list {
                        Ok(entries) => {
                            kernel::log!("store", "embedded kpkg: {} components", entries.len())
                        }
                        Err(e) => kernel::log!("store", "kpkg parse error: {:?}", e),
                    }
                } else {
                    kernel::log!("store", "store: not initialized");
                }

                // 转交 Core Monitor（boot hart 同步主循环，永不返回）
                kernel::monitor::run();
            }
            Err(e) => {
                panic!("core init failed: {}", e);
            }
        }
    }
}

/// 提取一个 FDT 节点的设备描述。
/// 过滤规则：必须有 reg 且 compatible 非空（memory 无 compatible、cpus/chosen/pmu 无 reg，天然跳过）。
type FdtParser<'a> = (
    fdt::parsing::unaligned::UnalignedParser<'a>,
    fdt::parsing::Panic,
);

fn device_descriptor<'a>(child: &fdt::nodes::Node<'a, FdtParser<'a>>) -> Option<DeviceDescriptor> {
    let mut descriptor = None;
    if let Some(r) = child.reg() {
        for entry in r.iter::<u64, u64>() {
            if let Ok(reg) = entry {
                descriptor = Some(DeviceDescriptor {
                    space: IoSpace::Mmio {
                        base: reg.address as usize,
                        size: reg.len as usize,
                    },
                    irq: None,
                    compatibles: [CompatStr::empty(); 4],
                    compat_count: 0,
                });
                break;
            }
        }
    }
    let mut d = descriptor?;
    if let Some(comp) = child.properties().find("compatible") {
        if let Ok(list) = comp.as_value::<StringList>() {
            for s in list {
                let idx = d.compat_count as usize;
                if idx < d.compatibles.len() {
                    d.compatibles[idx] = CompatStr::from_bytes(s.as_bytes());
                    d.compat_count += 1;
                }
            }
        }
    }
    if d.compat_count == 0 {
        return None;
    }
    if let Some(irqs) = child.properties().find("interrupts") {
        d.irq = irqs.as_value::<u32>().ok();
    }
    Some(d)
}

/// 把一批 FDT 子节点中符合规则的设备收集进 MachineInfo 的定长设备表。
fn collect_devices<'a>(
    children: impl IntoIterator<Item = fdt::nodes::Node<'a, FdtParser<'a>>>,
    devices: &mut [DeviceDescriptor; 26],
    dev_count: &mut usize,
) {
    for child in children {
        if let Some(d) = device_descriptor(&child) {
            if *dev_count < devices.len() {
                devices[*dev_count] = d;
                *dev_count += 1;
            }
        }
    }
}

struct DirectWriter;
impl core::fmt::Write for DirectWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            console::write_byte(byte);
        }
        Ok(())
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // 绕过 print（panic 时其锁可能已损坏），直接 SBI 紧急输出。
    let mut writer = DirectWriter;
    let _ = core::fmt::write(&mut writer, format_args!("\nPANIC: {}\n", _info));
    // This is the final fatal halt. Unlike ordinary error paths, there is no
    // caller to return to after the panic handler has reported the reason.
    loop {
        core::hint::spin_loop();
    }
}
