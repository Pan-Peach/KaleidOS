use crate::vm::{bootstrap, layout, runtime};
use alloc::vec::Vec;
use arch::CpuArch;
use core::arch::global_asm;
use core::num::NonZeroU64;
use core::panic::PanicInfo;
use kernel::machine::{
    CpuId, CpuInfo, FirmwareInfo, HardwareCpuId, MachineInfo, MemoryRegion, MAX_CPUS,
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
///
/// 低地址阶段**不**做完整 discovery、不构造 `MachineInfo`（更不分配）：它只做
/// 无堆内存 pass。这里是 hand-off 所需的纯输入；完整 discovery 在 high-half
/// entry 里、`memory::early_init` 之后进行。
#[repr(C)]
struct BootContext {
    /// OpenSBI 传下来的 DTB 物理地址（high-half 仍有 identity alias，可继续解析）。
    dtb_pa: usize,
    /// OpenSBI 选定的 boot hart（discovery 用它标 boot CPU）。
    hart_id: usize,
    /// 无堆 pass 选出的早期内存 arena（`memory::early_init` 的输入）。
    arena: MemoryRegion,
    /// 本文档镜像范围（Core 自己，永久 reserved）。
    reserved: [MemoryRegion; 1],
}

fn linker_addr(symbol: *const u8) -> usize {
    symbol as usize
}

#[cfg(feature = "machine")]
fn configure_machine_timer(info: &MachineInfo) {
    for device in info.devices.iter() {
        let kind = device
            .compatibles
            .iter()
            .find_map(|compatible| match &**compatible {
                "riscv,clint0" => Some(0x4000),
                "riscv,aclint-mtimer" => Some(0),
                _ => None,
            });
        let Some(offset) = kind else { continue };
        let Some((base, _)) = crate::discovery::primary_mmio(device) else {
            continue;
        };
        arch::riscv::firmware::configure_machine_timer(base + offset);
        return;
    }
    panic!("machine timer not found in device tree");
}

/// 把 discovery 找到的中断控制器（PLIC）基址交给 arch 机制（C6 骨架）。
/// 匹配真实 QEMU 的 `riscv,plic0` 与 fixture/新版的 `sifive,plic-1.0.0`；
/// 没有中断控制器的机器不阻塞 boot（外部中断不可用）。
fn configure_interrupt_controller(info: &MachineInfo) {
    for device in info.devices.iter() {
        if !crate::discovery::is_plic_device(device) {
            continue;
        }
        let Some((base, _)) = crate::discovery::primary_mmio(device) else {
            continue;
        };
        // 板级 PLIC context 计算留在 boot（QEMU virt：S-mode = hart*2+1，
        // M-mode = hart*2）；逐 CPU 填映射表，arch 不再写死该假设。
        let plic_context = |hardware: HardwareCpuId| -> usize {
            #[cfg(feature = "supervisor")]
            {
                hardware.raw() as usize * 2 + 1
            }
            #[cfg(feature = "machine")]
            {
                hardware.raw() as usize * 2
            }
        };
        const PLIC_SOURCE_COUNT: u32 = crate::discovery::PLIC_SOURCE_LIMIT;
        let mut contexts = [arch::riscv::plic::PlicCpuContext {
            cpu: CpuId::from_raw(0),
            context: 0,
        }; arch::riscv::plic::MAX_PLIC_CONTEXTS];
        let count = info
            .cpu_info
            .len()
            .min(arch::riscv::plic::MAX_PLIC_CONTEXTS);
        for (i, slot) in contexts.iter_mut().enumerate().take(count) {
            *slot = arch::riscv::plic::PlicCpuContext {
                cpu: CpuId::from_raw(i),
                context: plic_context(info.cpu_info[i].hardware_id),
            };
        }
        // SAFETY: base 来自已发现的 PLIC MMIO 窗口。
        if unsafe {
            <arch::InterruptImpl as arch::InterruptController>::configure(
                arch::riscv::plic::PlicConfig {
                    base,
                    contexts,
                    context_count: count,
                    external_cpu: CpuId::from_raw(0),
                    source_count: PLIC_SOURCE_COUNT,
                },
            )
        }
        .is_err()
        {
            kernel::log!("discovery", "PLIC configure rejected");
        }
        return;
    }
    kernel::log!("discovery", "no PLIC found; external IRQ unavailable");
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
static INITPKG: [u8; include_bytes!(env!("KALEIDOS_INITPKG")).len()] =
    *include_bytes!(env!("KALEIDOS_INITPKG"));

/// OpenSBI 选定的 boot hart 进入 payload；其它 hart 留在 firmware 的 warm-boot 路径。
#[unsafe(no_mangle)]
extern "C" fn bootstrap_main(hart_id: usize, dtb_pa: usize, kernel_pa: usize) -> ! {
    arch::CpuImpl::init_cpu();
    // 统一 trap 约定：给 CPU0 装入口记录（`sscratch` = `&entry` = trap 栈顶）。
    // UP 与 SMP 同一条路径；SMP 时每个 AP 在 Core 的 `secondary_entry` 各自装。
    // 必须在任何可能 trap 之前完成。`base` 暂用占位（Core 的 per-CPU 存储尚未接）。
    unsafe {
        <arch::CpuImpl as arch::CpuArch>::install_per_cpu_base(
            CpuId::from_raw(0),
            core::ptr::NonNull::dangling(),
        );
    }
    kernel::log!("bootstrap", "arch init OK");
    kernel::log!("bootstrap", "KaleidOS bootstrap");
    kernel::log!("bootstrap", "========================================");

    // FDT 发现：直接吃 OpenSBI 给的 dtb 物理地址（unsafe：该地址有效性 Rust 无从验证）
    let tree = match unsafe { fdt::Fdt::from_ptr_unaligned(dtb_pa as *const u8) } {
        Ok(tree) => {
            kernel::log!("bootstrap", "FDT magic: OK");
            tree
        }
        Err(_) => panic!("FDT magic: BAD"),
    };

    // 无堆早期内存 pass（低地址阶段**不**分配、不构造 MachineInfo）：
    // 镜像范围 → 包含镜像的 bank（找不到即失败，不落回无关 bank）→ 排除镜像与
    // 所有 FDT live/reserved 区间后的 arena。
    let linked_image_start =
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__bootstrap_start)));
    let linked_image_end =
        bootstrap::physical_address_of(linker_addr(core::ptr::addr_of!(__bootstrap_end)));
    let image_size = linked_image_end
        .checked_sub(linked_image_start)
        .expect("invalid linked kernel image range");
    let image = MemoryRegion {
        base: kernel_pa,
        size: image_size,
    };
    let bank = crate::bootmem::image_bank(&tree, kernel_pa, image_size)
        .unwrap_or_else(|error| panic!("Sv39 early memory: {}", error));
    // KernelNative 组件从 Core 堆取镜像并做 ±2 GiB PC-relative 重定位：arena
    // 收在镜像可达窗口内（见 `bootmem::arena_search_window`）。
    let window = crate::bootmem::arena_search_window(bank, image);
    let arena = kernel::memory::select_arena(window, image, |emit| {
        emit(image)?;
        crate::bootmem::scan_fdt_exclusions(&tree, dtb_pa, emit)
    })
    .unwrap_or_else(|error| panic!("Sv39 early memory: {}", error));
    kernel::log!(
        "bootstrap",
        "early arena {:#x}-{:#x} ({} KiB), bank {:#x}+{} MiB",
        arena.base,
        arena.base + arena.size,
        arena.size / 1024,
        bank.base,
        bank.size / (1024 * 1024)
    );

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
            bank.base,
            bank.size,
            &layout.sections(),
        )
    } {
        Ok(()) => {}
        Err(error) => panic!("Sv39 early map failed: {:?}", error),
    }

    // 本文档镜像范围 → reserved（Core 自己，永久保留）
    let context = BootContext {
        dtb_pa,
        hart_id,
        arena,
        reserved: [MemoryRegion {
            base: kernel_pa,
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

/// 完整 discovery（high-half、`early_init` 之后）：FDT → owned `MachineInfo`。
///
/// 用 `Vec` 收集（**不**截断到旧定长容量），交接处 `.into_boxed_slice()`；
/// compatible 字符串 owned（`Box<str>`，无 32B / 4 槽截断）。**FDT 源不再可丢**：
/// `FirmwareInfo::Fdt`
/// 永久保留该物理区间（arena 选择已把它排除），供未来驱动读原始视图。
/// CPU 承认规则：BSP 归一到逻辑
/// CPU0；firmware 描述的 CPU 超过 `MAX_CPUS` 时 BSP 优先、其余按发现顺序取前
/// `MAX_CPUS` 并显式诊断。**BSP 缺失时不制造**——Core 校验会拒绝该提案。
fn discover<'a>(
    tree: &fdt::Fdt<'a, crate::discovery::FdtParser<'a>>,
    hart_id: usize,
    firmware: FirmwareInfo,
) -> Result<MachineInfo, &'static str> {
    let mut memory_regions: Vec<MemoryRegion> = Vec::new();
    for region in tree.root().memory().reg().iter::<u64, u64>() {
        let Ok(r) = region else { continue };
        memory_regions.push(MemoryRegion {
            base: r.address as usize,
            size: r.len as usize,
        });
    }

    let mut cpu_info: Vec<CpuInfo> = Vec::new();
    for cpu in tree.root().cpus().iter() {
        let hart = cpu.reg::<u64>().first().unwrap_or(0);
        cpu_info.push(CpuInfo {
            boot_cpu: hart == hart_id as u64,
            hardware_id: HardwareCpuId::from_raw(hart),
        });
    }

    // 归一化：逻辑 CPU id = 数组下标（从 0 稠密），并强制 **boot hart = 逻辑
    // CPU0**。OpenSBI 用抽签选 boot hart，它不一定是 discovery 下标 0；不归一
    // 化时 BSP 会绑到 CPU0 的入口记录/trap 栈，而某个 AP 之后又占用 CPU0 →
    // 逻辑身份互相别名（smp-percpu 因此 flaky）。保持「BSP = 逻辑 0」这一
    // 既有不变式，PLIC 外部固定路由、`trap_stack_*`、per-CPU 表全部继续正确。
    if let Some(boot_index) = cpu_info.iter().position(|c| c.boot_cpu) {
        cpu_info.swap(0, boot_index);
    }
    if cpu_info.len() > MAX_CPUS {
        kernel::log!(
            "discovery",
            "too many CPUs ({}); admitting {} (BSP first)",
            cpu_info.len(),
            MAX_CPUS
        );
        cpu_info.truncate(MAX_CPUS);
    }

    // 两层遍历：root 挂系统级设备（QEMU 把 fw-cfg/flash 放在 /），/soc 挂总线设备；
    // 完整中断资源解析 + PLIC 逻辑线绑定都在 discovery 模块（RV64/RV32 共用）。
    let devices = crate::discovery::collect_devices(tree)?;

    Ok(MachineInfo {
        boot_hardware_id: HardwareCpuId::from_raw(hart_id as u64),
        // FDT 报告的 timebase 速率：非零才算已知（`None` = 未报告 / 报 0）。
        timebase_frequency: tree
            .root()
            .cpus()
            .common_timebase_frequency()
            .and_then(NonZeroU64::new),
        firmware,
        cpu_info: cpu_info.into_boxed_slice(),
        memory_regions: memory_regions.into_boxed_slice(),
        devices: devices.into_boxed_slice(),
    })
}

/// First Rust entry reached through the high-half alias.
///
/// The identity mapping is still present only as a bootstrap safety alias;
/// this function and the rest of the kernel are already linked in the high
/// VMA, with their bytes loaded at the low physical LMA.
#[unsafe(no_mangle)]
extern "C" fn bootstrap_high(context_ptr: usize) -> ! {
    arch::CpuImpl::init_cpu();

    let context = unsafe { &*(context_ptr as *const BootContext) };
    kernel::log!("bootstrap", "entered high-half kernel");
    print_linker_layout();

    // Early-memory seam：无堆 pass 已选定 arena；在完整 discovery / 任何分配
    // 之前把它交给 Core（一次性，重复调用被拒绝）。
    if let Err(error) = unsafe { kernel::memory::early_init(context.arena) } {
        panic!("early memory init failed: {}", error);
    }

    // 完整 discovery：high-half 仍有 identity alias，FDT 物理地址可直接解析。
    let tree = match unsafe { fdt::Fdt::from_ptr_unaligned(context.dtb_pa as *const u8) } {
        Ok(tree) => tree,
        Err(error) => panic!("FDT parse failed in high half: {:?}", error),
    };
    // 保留的原始固件源 = 入口给出的 **原始** DTB 物理区间（fdt parser 已验证的
    // totalsize）；arena 选择已把该区间永久排除，boot 生命周期内不回收。
    let firmware = FirmwareInfo::Fdt {
        phys: context.dtb_pa,
        size: tree.total_size(),
    };
    let info = discover(&tree, context.hart_id, firmware)
        .unwrap_or_else(|error| panic!("discovery failed: {}", error));
    kernel::log!("discovery", "firmware: {:?}", info.firmware);

    #[cfg(feature = "machine")]
    configure_machine_timer(&info);
    configure_interrupt_controller(&info);

    kernel::log!("bootstrap", "MachineInfo dump:");
    kernel::printk!("{:#?}\n", info);
    kernel::log!("bootstrap", "BOOT DISCOVERY OK");

    // Core 消费提案（owned）并返回唯一提交的 `&'static` 快照：此后 boot 侧
    // 一律借用它（runtime VM / SMP / selftest / monitor）。
    let info = match kernel::init(info, &context.reserved) {
        Ok(info) => info,
        Err(error) => panic!("core init failed: {}", error),
    };

    // 长期内核地址空间：buddy 已活（kernel::init 之后），建立/校验/激活/全局
    // 安装 runtime root，替换 bootstrap 临时 root。
    // kernel_pa = 镜像加载地址（context.reserved[0] 即镜像 PA 范围）。
    let runtime_layout = layout::kernel_layout();
    runtime::init(&runtime_layout, context.reserved[0].base, info)
        .expect("Sv39 runtime VM init failed");
    kernel::log!("mmu", "runtime VM active");

    // 保留的 FDT 源必须在早期分配（buddy 页表 / discovery Vec）之后仍可读：
    // 重读 header 的 magic 与 totalsize。runtime root 对 RAM 的 identity 映射
    // 覆盖该区间；若 FDT 在 `memory_regions` 之外，`vm::runtime::build` 已额外
    // 建立 Core 可访问映射——物理驻留与可读性一起保留。
    if let FirmwareInfo::Fdt { phys, size } = info.firmware {
        if !crate::bootmem::retained_fdt_intact(phys, size) {
            panic!(
                "retained FDT is unreadable after early allocations (phys {:#x}, size {})",
                phys, size
            );
        }
        kernel::log!(
            "discovery",
            "retained FDT intact at {:#x} ({} bytes)",
            phys,
            size
        );
    }

    // 内嵌组件仓库：.initpkg section = init.kpkg（cpio 归档）
    let pkg_start = core::ptr::addr_of!(__initpkg_start) as usize;
    let pkg_end = core::ptr::addr_of!(__initpkg_end) as usize;
    let pkg = unsafe { core::slice::from_raw_parts(pkg_start as *const u8, pkg_end - pkg_start) };
    kernel::component::store::init(pkg);

    // 显式开全局中断：各本地源已在 kernel::init 中解源。
    arch::CpuImpl::enable_irq();

    #[cfg(feature = "selftest")]
    {
        // selftest 在**完整初始化之后**运行：Core + 长期内核地址空间
        // （device MMIO 才被映射）都就绪，才能测 PLIC/UART 这类真实设备契约。
        crate::selftest::run(info);
    }

    #[cfg(not(feature = "selftest"))]
    {
        kernel::log!("core", "BOOT CORE OK");
        if let Some(store) = kernel::component::store::get_component_store() {
            let list = store.list();
            match list {
                Ok(entries) => {
                    // 只数组件：cpio 归档里还有 manifest 等元数据条目。
                    let components = entries
                        .iter()
                        .filter(|entry| entry.name.ends_with(b".kcomp"))
                        .count();
                    kernel::log!("store", "embedded kpkg: {} components", components)
                }
                Err(e) => kernel::log!("store", "kpkg parse error: {:?}", e),
            }
        } else {
            kernel::log!("store", "store: not initialized");
        }

        // boot hart 在全局中断已开、长期地址空间已生效之后，才启动次 CPU
        // （见 src/smp.rs）。单 CPU 机器上自然空转。
        crate::smp::start_secondaries(info);
        crate::composition::start();
        // 转交 Core Monitor（boot hart 同步主循环，永不返回）
        kernel::monitor::run();
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
    // 组件 panic：先打印一行诊断（直接 SBI 字节输出，绕过 printk 锁、无分配、
    // 无锁），再逃逸到 Core 保存的上下文。诊断信息（message/location）只有
    // boot panic handler 拿得到，所以打印必须在这里。
    if let Some(escape) = kernel::component::containment::active_escape() {
        let mut writer = DirectWriter;
        let message = _info.message();
        let _ = kernel::component::containment::write_escape_line(
            &mut writer,
            escape,
            _info.location(),
            Some(&message),
        );
    }
    if kernel::component::panic_escape() {
        loop {
            core::hint::spin_loop();
        }
    }
    // 绕过 print（panic 时其锁可能已损坏），直接 SBI 紧急输出。
    let mut writer = DirectWriter;
    let _ = core::fmt::write(&mut writer, format_args!("\nPANIC: {}\n", _info));
    // This is the final fatal halt. Unlike ordinary error paths, there is no
    // caller to return to after the panic handler has reported the reason.
    loop {
        core::hint::spin_loop();
    }
}
