//! KaleidOS 内核 —— Resource Core（真相：存在性/状态/所有权/生命周期）。
//!
//! 依赖方向：本 crate（Resource Core **library**）不依赖具体 ISA 实现、Discovery backend、
//! 组件或 bootstrap；它只依赖 `os/arch` 提供的稳定 backend contract，消费归一化的
//! MachineInfo（`machine::MachineInfo`），
//! 不知道 FDT / ACPI / QEMU / 板子，也不知道加载器是谁。
//! 本 crate 是 host-testable 的 library（`cargo test` 专用）；运行时常与 bootstrap 阶段
//! 一起链接成 `kaleidos.elf`（单镜像，职责分离装载合一，见 `docs/architecture/overview.md` §3）。
//! ISA backend（`os/arch`）与 FDT 解析（`third_party/fdt` 子模块）由 bootstrap 组合。
//! 设计契约见 `docs/architecture/overview.md` 与 `docs/philosophy/core-philosophy.md`。

#![no_std]
extern crate alloc;

#[cfg(test)]
extern crate std;

// build.rs ↔ Kconfig 的窄契约（`TRACE_CAPACITY` 解析 / 校验）只在 host test 下
// 编译进 lib：测试锁定的是 build.rs 实际用的那一份实现（见 `src/build_config.rs`）。
#[cfg(test)]
mod build_config;

// host 测试锁的规范顺序 + 违规检测（详见 `src/test_support.rs` 模块文档）。
#[cfg(test)]
mod test_support;

pub mod bench;
pub mod component;
pub mod errno;
pub mod generated;
pub mod irq;
pub mod machine;
pub mod memory;
pub mod monitor;
#[macro_use]
pub mod print;
pub mod resource;
pub mod sched;
pub mod smp;
pub mod task;
pub mod timer;
pub mod trace;

// 裸机目标才接管全局分配器；host test（std 环境）用 std 默认分配器。
// KernelAllocator 是 ISA 中立的 Core 机制，所有裸机后端（RISC-V / x86_64 /
// AArch64）共用；新增裸机 ISA 时必须在此登记，否则 `alloc` 无法链接。
#[cfg(all(
    not(test),
    target_os = "none",
    any(
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "x86_64",
        target_arch = "aarch64"
    )
))]
#[global_allocator]
static ALLOCATOR: memory::KernelAllocator = memory::KernelAllocator;

/// Core 初始化入口：消费 bootstrap 发现的 MachineInfo（提案），校验后提交资源真相。
/// `reserved` 是需保留的区间（bootstrap 提供：ELF image range）。
///
/// **不初始化 / 不重置内存**：分配器由 boot 的早期内存 seam
/// （`memory::early_init(arena)`，无堆 pass 选出的单一 arena）在 `init` 之前
/// 启动；seam 未跑即 fail-closed。镜像范围必须落在已发现的 RAM region 内
/// （arena 选择与长期映射的前提，input sanity）。
pub fn init(
    info: &machine::MachineInfo,
    reserved: &[machine::MemoryRegion],
) -> Result<(), &'static str> {
    if !memory::is_initialized() {
        return Err("memory not early-initialized");
    }
    if info.mem_count == 0 {
        return Err("no memory regions");
    }
    if info.mem_count > info.memory_regions.len()
        || info.cpu_count > info.cpu_info.len()
        || info.dev_count > info.devices.len()
    {
        return Err("machine info count exceeds capacity");
    }
    if info.cpu_count == 0 {
        return Err("no cpu info");
    }
    if !info.cpu_info[..info.cpu_count]
        .iter()
        .any(|cpu| cpu.hardware_id == info.boot_hardware_id)
    {
        return Err("boot hart is not present in cpu info");
    }

    // 镜像（reserved）必须被某个已发现 RAM region 完整覆盖：boot 的 arena
    // 选择与长期映射都以该不变式为前提，Core 不采信未验证的布局输入。
    // 前提：BSS 已在 bootstrap 启动汇编里清零（见 entry.S）；
    // core 不再负责 BSS 清零（那是启动路径职责）。
    if let (Some(first), Some(last)) = (reserved.first(), reserved.last()) {
        let image_start = first.base;
        let image_end = last
            .base
            .checked_add(last.size)
            .ok_or("reserved region overflows")?;
        let covered = info.memory_regions[..info.mem_count].iter().any(|region| {
            region.base <= image_start
                && region
                    .base
                    .checked_add(region.size)
                    .is_some_and(|end| image_end <= end)
        });
        if !covered {
            return Err("reserved image is outside the discovered RAM regions");
        }
    }

    task::init();
    sched::init();
    component::containment::init();
    #[cfg(feature = "preempt")]
    timer::init_preempt(info.timebase_frequency as usize).map_err(|_| "timer init failed")?;
    #[cfg(not(feature = "preempt"))]
    if let Err(error) = timer::init() {
        // 协作式 profile 可以在**没有 timer 投递**的情况下继续：`print::
        // idle_wait` 会在 arm 失败时回退轮询，绝不挂死。需要 timer 驱动的
        // profile（`preempt`，上面那行）仍然 fail-closed。
        log!(
            "timer",
            "delivery unavailable ({:?}); idle falls back to polling",
            error
        );
    }

    component::registry::init();
    component::endpoint::init();
    resource::init();
    irq::init();
    // SMP：BSP 侧 Core 初始化（发布 CPU 记录、注册 Core IPI 回调、BSP Online）。
    // **不**启动 AP、**不**开 IPI 源——物理启动仍由 boot 在长期地址空间就绪后触发，
    // 且接收端应答/drain 落地前不得开源（见 `os/core/src/smp/mod.rs` 模块文档）。
    smp::init(info).map_err(|_| "smp init failed")?;
    log!("core", "init OK");
    monitor::mount(info);
    Ok(())
}
