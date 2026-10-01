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

/// Core 初始化入口：**消费** bootstrap 发现的 `MachineInfo`（owned 提案），
/// 校验后一次性提交（[`machine::commit`]），再从已提交快照初始化其余子系统，
/// 返回该 `&'static` 真相供 boot 后续消费（runtime VM / SMP / 监视器）。
///
/// `reserved` 是需保留的区间（bootstrap 提供：ELF image range）。
///
/// **不初始化 / 不重置内存**：分配器由 boot 的早期内存 seam
/// （`memory::early_init(arena)`，无堆 pass 选出的单一 arena）在 `init` 之前
/// 启动；seam 未跑即 fail-closed。镜像范围必须落在已发现的 RAM region 内
/// （arena 选择与长期映射的前提，input sanity）。
///
/// **发布之后的任何失败都是终止启动**：没有 retry、没有替换 API。
pub fn init(
    info: machine::MachineInfo,
    reserved: &[machine::MemoryRegion],
) -> Result<&'static machine::MachineInfo, &'static str> {
    if !memory::is_initialized() {
        return Err("memory not early-initialized");
    }
    validate_proposal(&info, reserved)?;
    let info = machine::commit(info)?;

    task::init();
    sched::init();
    component::containment::init();
    // 抢占必须知道 timebase 速率：未知（`None`）时 `init_preempt` fail-closed，
    // 这里把失败升级为启动失败（没有可用的调度 tick，不假装成功）。
    #[cfg(feature = "preempt")]
    timer::init_preempt(info.timebase_frequency).map_err(|_| "timer init failed")?;
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
    // resource 表按已提交快照的**设备数**定容（长度即真相，全宽 DeviceId）。
    resource::init();
    irq::init();
    // SMP：BSP 侧 Core 初始化（发布 CPU 记录、注册 Core IPI 回调、BSP Online）。
    // **不**启动 AP、**不**开 IPI 源——物理启动仍由 boot 在长期地址空间就绪后触发，
    // 且接收端应答/drain 落地前不得开源（见 `os/core/src/smp/mod.rs` 模块文档）。
    smp::init(info).map_err(|_| "smp init failed")?;
    log!("core", "init OK");
    Ok(info)
}

/// 校验机器提案的形状（`commit` 之前）：RAM / CPU / 设备表的存在性、CPU 上限
/// 与 BSP 不变式、硬件身份唯一性、设备表可被 `DeviceId`（`u32`）索引、
/// reserved 镜像落在已发现 RAM 内。
///
/// **长度即真相**：所有迭代走完整切片，没有 count 截断。BSP 必须由发现阶段
/// 归一化到逻辑 CPU0（唯一且与 `boot_hardware_id` 一致）；Core **不制造** BSP，
/// 缺失即拒绝。
fn validate_proposal(
    info: &machine::MachineInfo,
    reserved: &[machine::MemoryRegion],
) -> Result<(), &'static str> {
    if info.memory_regions.is_empty() {
        return Err("no memory regions");
    }
    if info.cpu_info.is_empty() {
        return Err("no cpu info");
    }
    if info.cpu_info.len() > machine::MAX_CPUS {
        return Err("cpu info exceeds MAX_CPUS");
    }
    // 设备身份是 u32（DeviceId）：表长必须能被 u32 索引（64 位目标上的防御；
    // 32 位目标上 usize == u32，转换恒成功）。
    if u32::try_from(info.devices.len()).is_err() {
        return Err("device table exceeds DeviceId width");
    }

    // 保留的固件源是**位置真相**：非 `Static` 必须是形状合法的保留区间。
    // 内容校验（FDT header / RSDP 签名·校验和·长度）在 boot 边界完成；Core
    // 拒绝零地址 / 零长度这类不可能合法的提案——绝不发布指向无效字节的固件根。
    match info.firmware {
        machine::FirmwareInfo::Fdt { phys, size } => {
            if phys == 0 || size == 0 {
                return Err("invalid retained FDT source");
            }
        }
        machine::FirmwareInfo::Acpi { rsdp } => {
            if rsdp == 0 {
                return Err("invalid retained ACPI RSDP source");
            }
        }
        machine::FirmwareInfo::Static => {}
    }

    // BSP：**唯一**、位于逻辑 CPU0、硬件身份与 `boot_hardware_id` 一致。
    let boot_count = info.cpu_info.iter().filter(|cpu| cpu.boot_cpu).count();
    if boot_count != 1
        || !info.cpu_info[0].boot_cpu
        || info.cpu_info[0].hardware_id != info.boot_hardware_id
    {
        return Err("boot CPU is not the unique logical CPU0");
    }

    // 硬件身份不得重复：重复会把两个逻辑 CPU 指向同一物理 hart。
    for (index, cpu) in info.cpu_info.iter().enumerate() {
        if info.cpu_info[index + 1..]
            .iter()
            .any(|other| other.hardware_id == cpu.hardware_id)
        {
            return Err("duplicate hardware cpu id");
        }
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
        let covered = info.memory_regions.iter().any(|region| {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use machine::{CpuInfo, DeviceDescriptor, HardwareCpuId, MachineInfo, MemoryRegion};

    fn cpu(boot: bool, hardware: u64) -> CpuInfo {
        CpuInfo {
            boot_cpu: boot,
            hardware_id: HardwareCpuId::from_raw(hardware),
        }
    }

    fn ram(base: usize, size: usize) -> MemoryRegion {
        MemoryRegion { base, size }
    }

    fn proposal(cpus: Vec<CpuInfo>, regions: Vec<MemoryRegion>) -> MachineInfo {
        machine::test_support::snapshot(
            HardwareCpuId::from_raw(0),
            core::num::NonZeroU64::new(10_000_000),
            cpus,
            regions,
            vec![DeviceDescriptor::empty()],
        )
    }

    #[test]
    fn validate_rejects_empty_cpu_or_ram_inventory() {
        // 空 CPU 表。
        assert_eq!(
            validate_proposal(&proposal(vec![], vec![ram(0x8000_0000, 0x1000)]), &[]),
            Err("no cpu info")
        );
        // 空 RAM inventory（设备表可为空，但 RAM 不行）。
        assert_eq!(
            validate_proposal(&proposal(vec![cpu(true, 0)], vec![]), &[]),
            Err("no memory regions")
        );
    }

    #[test]
    fn validate_enforces_max_cpus_as_an_admitted_cpu_limit() {
        let mut cpus = Vec::new();
        cpus.push(cpu(true, 0));
        for hardware in 1..=machine::MAX_CPUS {
            cpus.push(cpu(false, hardware as u64));
        }
        // MAX_CPUS + 1 台：拒绝（长度即限制）。
        assert_eq!(
            validate_proposal(&proposal(cpus, vec![ram(0x8000_0000, 0x1000)]), &[]),
            Err("cpu info exceeds MAX_CPUS")
        );
    }

    #[test]
    fn validate_requires_a_unique_boot_cpu_at_logical_zero() {
        // BSP 不在逻辑 0（发现阶段未归一化）。
        assert_eq!(
            validate_proposal(
                &proposal(
                    vec![cpu(false, 0), cpu(true, 1)],
                    vec![ram(0x8000_0000, 0x1000)]
                ),
                &[]
            ),
            Err("boot CPU is not the unique logical CPU0")
        );
        // boot_cpu 标记在 0，但硬件身份与 boot_hardware_id 不一致（不能制造 BSP）。
        let mut info = proposal(vec![cpu(true, 7)], vec![ram(0x8000_0000, 0x1000)]);
        info.boot_hardware_id = HardwareCpuId::from_raw(99);
        assert_eq!(
            validate_proposal(&info, &[]),
            Err("boot CPU is not the unique logical CPU0")
        );
        // 两个 boot_cpu 标记：不唯一。
        assert_eq!(
            validate_proposal(
                &proposal(
                    vec![cpu(true, 0), cpu(true, 1)],
                    vec![ram(0x8000_0000, 0x1000)]
                ),
                &[]
            ),
            Err("boot CPU is not the unique logical CPU0")
        );
    }

    #[test]
    fn validate_rejects_duplicate_hardware_ids() {
        assert_eq!(
            validate_proposal(
                &proposal(
                    vec![cpu(true, 0), cpu(false, 0)],
                    vec![ram(0x8000_0000, 0x1000)]
                ),
                &[]
            ),
            Err("duplicate hardware cpu id")
        );
    }

    /// reserved 覆盖检查走**完整** RAM 切片（不只是前缀）：镜像落在第 17 个
    /// region 里也必须通过——旧的 16 项 inventory 上限已删除。
    #[test]
    fn validate_checks_reserved_against_the_full_ram_inventory() {
        let regions: Vec<MemoryRegion> = (0..20)
            .map(|i| ram(0x8000_0000 + i * 0x10000, 0x10000))
            .collect();
        let image = ram(0x8000_0000 + 17 * 0x10000, 0x10000);
        assert_eq!(
            validate_proposal(&proposal(vec![cpu(true, 0)], regions.clone()), &[image]),
            Ok(())
        );
        // 未覆盖的镜像仍然拒绝。
        assert_eq!(
            validate_proposal(
                &proposal(vec![cpu(true, 0)], regions),
                &[ram(0x4000_0000, 0x1000)]
            ),
            Err("reserved image is outside the discovered RAM regions")
        );
    }

    /// 保留的固件源必须是形状合法的位置：零地址 / 零长度 → 拒绝提案（绝不以
    /// `Static` 之外的形式发布悬空固件根）；`Static` 不携带来源，合法。
    #[test]
    fn validate_rejects_dangling_firmware_sources() {
        let mut info = proposal(vec![cpu(true, 0)], vec![ram(0x8000_0000, 0x1000)]);
        assert_eq!(validate_proposal(&info, &[]), Ok(()), "Static 合法");

        info.firmware = machine::FirmwareInfo::Fdt {
            phys: 0,
            size: 0x1000,
        };
        assert_eq!(
            validate_proposal(&info, &[]),
            Err("invalid retained FDT source")
        );

        info.firmware = machine::FirmwareInfo::Fdt {
            phys: 0x8000_0000,
            size: 0,
        };
        assert_eq!(
            validate_proposal(&info, &[]),
            Err("invalid retained FDT source")
        );

        info.firmware = machine::FirmwareInfo::Acpi { rsdp: 0 };
        assert_eq!(
            validate_proposal(&info, &[]),
            Err("invalid retained ACPI RSDP source")
        );

        info.firmware = machine::FirmwareInfo::Acpi { rsdp: 0xf_0000 };
        assert_eq!(validate_proposal(&info, &[]), Ok(()));
    }
}
