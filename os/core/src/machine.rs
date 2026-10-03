//! 归一化机器信息（MachineInfo）：bootstrap 发现 → core::init 消费。
//! 单镜像内函数调用交接，用 Rust 类型即可（无需跨 binary POD/协议）。
//!
//! **owned 不可变快照**：bootstrap 在完整 discovery 里用 `Vec` 复制出需要的
//! 内容，交接处 `.into_boxed_slice()`；`core::init` 校验后**一次性提交**，
//! 此后全系统只通过 [`committed`] 读同一份 `&'static MachineInfo`。快照
//! 不实现 `Clone` / `Copy`：没有自动深拷贝，也没有发布后的替换 / 重置 API
//! （发布后失败 = 终止启动，不 retry）。
//!
//! **长度即真相**：`cpu_info.len()` / `memory_regions.len()` / `devices.len()`；
//! 没有 `*_count` 字段。`memory_regions` 是发现的 **RAM inventory**（不是空闲
//! 内存，也不是分配器 arena），Core 不把它改成"当前可分配"的列表。
//!
//! `firmware` 是**保留的原始固件描述源**（[`FirmwareInfo`]）：归一化视图不是
//! 硬件描述的终点，未来驱动可据此读取 vendor 特定数据；保留字节不回收。
//!
//! 字段语义：Core Resource Truth 的"提案"，由 core::init 校验后提交。

use alloc::boxed::Box;
use core::fmt::Debug;
use core::num::NonZeroU64;

// 逻辑 CPU 身份（`CpuId`）与硬件 CPU 身份（`HardwareCpuId`）定义在 `arch`：
// arch 的 trait 签名需要逻辑 id，而 arch 不依赖 core。这里 re-export，
// `core::machine::CpuId` 仍是 Core 侧唯一入口（契约不变）。
pub use arch::cpu::{CpuId, HardwareCpuId};

/// 一个被发现的 CPU：Core 按数组下标赋**逻辑** id（`CpuInfo` 的顺序即逻辑序），
/// 而 `hardware_id` 是 discovery 报来的**硬件**身份（hartid/APIC/MPIDR/CPUID）。
///
/// 二者不同：硬件 id 可能稀疏、非零起点，**绝不能**当数组下标用。
#[derive(Debug, Clone, Copy)]
pub struct CpuInfo {
    pub boot_cpu: bool,
    pub hardware_id: HardwareCpuId,
}

/// **已承认（admitted）逻辑 CPU 数**的编译期上限：`1 <= cpu_info.len() <= MAX_CPUS`。
///
/// 这不是运行时真值（真值 = `MachineInfo.cpu_info.len()`）；SMP 的 `CpuMask` /
/// per-CPU 索引以本常量为上界。发现阶段若 firmware 描述更多 CPU，boot 必须
/// **BSP 优先、其余按发现顺序**取前 `MAX_CPUS` 台并显式诊断；Core 不制造 CPU。
///
/// 定义在叶子 crate `arch`（由 Kconfig `MAX_CPUS` 经 `arch/build.rs` 生成），
/// 这里只 re-export：`core` 依赖 `arch`，常量必须落在 `arch` 才能被两边共用。
/// 要支持更多 CPU，改 Kconfig `MAX_CPUS` 一处即可。
pub use arch::MAX_CPUS;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    pub base: usize,
    pub size: usize,
}

fn write_size(f: &mut core::fmt::Formatter<'_>, bytes: usize) -> core::fmt::Result {
    let bytes = bytes as u64;
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if bytes >= GIB && bytes.is_multiple_of(GIB) {
        write!(f, "{} GiB", bytes / GIB)
    } else if bytes >= MIB && bytes.is_multiple_of(MIB) {
        write!(f, "{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes.is_multiple_of(KIB) {
        write!(f, "{} KiB", bytes / KIB)
    } else {
        write!(f, "{} B", bytes)
    }
}

impl core::fmt::Debug for MemoryRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "MemoryRegion {{ base: {:#x}, size: ", self.base)?;
        write_size(f, self.size)?;
        write!(f, " }}")
    }
}

/// **保留的原始固件描述源**：boot 校验后保留的字节在哪里、有多少。
///
/// 归一化的 [`MachineInfo`] 不是硬件描述的终点：未来驱动可能需要 vendor
/// 特定数据（FDT 节点 / ACPI 表）。本类型只给出**已验证的来源位置**：
/// - 只说明"字节在哪、有多大"，**不认证内容**；下游表（RSDT/XSDT/…）由
///   未来消费者在读取时各自校验；
/// - 保留字节不回收（本阶段没有 reclaim 机制）：其所在区间由 boot 的 arena
///   选择永久排除；
/// - **不是** `repr(C)`、不进 `kcore_*` / 组件 SDK —— boot + Core 内部真相。
///
/// `Static` = 本机**没有保留的受支持固件描述**（不是"校验失败但继续"）。
#[derive(Clone, Copy, Debug)]
pub enum FirmwareInfo {
    /// 保留的 FDT：`phys` 是本执行域可直接读的地址（RISC-V = DTB 物理区间；
    /// AArch64 = staging 副本地址），`size` 是已验证的 `totalsize`。
    Fdt { phys: usize, size: usize },
    /// 保留的 ACPI RSDP 地址（签名 / 校验和 / 长度已验证）。
    Acpi { rsdp: usize },
    /// 没有保留的受支持固件描述。
    Static,
}

/// 设备的一个空间条目：MMIO 窗口或 PIO 窗口，互斥由类型保证。
/// x86 特有 PIO（RISC-V/ARM 只有 MMIO）；一个设备可占多条目（如 PCI 双 BAR）。
/// 预留：X86_64 arch 发 Pio 条目，Core 的 Handle 机制据此选择访问原语。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoSpace {
    Mmio { base: usize, size: usize },
    Pio { base: usize, size: usize },
}

/// 一条中断资源的**固件 specifier**：哪个中断控制器 + 一条完整 specifier。
///
/// `specifier` 与 [`InterruptResource::line`] 是**两个不同的事实**：
/// specifier 是固件怎么描述这条中断（控制器身份 + 原始 cells），line 是 Core
/// 的后端能不能把它变成一条可投递的**逻辑外部 IRQ 号**。绝不把解码后的
/// GIC INTID / 向量 / 固件 cell 直接当逻辑 IRQ 号用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterruptSpecifier {
    /// FDT 的一条完整中断 tuple：
    /// - `controller` 是**已解析**（resolved）的 interrupt controller phandle
    ///   （host-endian；`interrupts` 经继承的 `interrupt-parent` 解析，
    ///   `interrupts-extended` 每条 tuple 自带 phandle）；
    /// - `cells` 是该控制器 `#interrupt-cells` 规定的**一条** specifier
    ///   （host-endian，**不含** phandle）——完整保留 GIC 的 type/number/flags
    ///   等全部 cell，不是属性包。
    Fdt { controller: u32, cells: Box<[u32]> },
    /// ISA IRQ 线（x86 8259 风格的固件编号）。
    Isa { line: u8 },
}

/// 设备的一条中断资源：固件 specifier + Core 可投递的**逻辑 IRQ 号**（或未绑定）。
///
/// `line: None` = 资源被完整保留但本阶段后端无法投递（例如无 GIC/PIC 路由，
/// 或 PLIC 源不在配置范围内）；这不是"没有中断"，是"有、但当前不可路由"。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterruptResource {
    pub specifier: InterruptSpecifier,
    pub line: Option<u32>,
}

/// 设备发现记录（owned；随 `MachineInfo` 一次性发布，**不再 `Copy`**）。
///
/// - `spaces` 是设备的**全部**空间窗口，按固件顺序；`spaces[0]` 是**主窗口**
///   （当前 `kcore_device_claim` 只返回它——多窗口由 boot/Core 保留与映射，
///   本阶段没有组件侧的索引窗口 API）。一条设备可以有多个 MMIO/PIO 窗口
///   （如 PCI 双 BAR）。
/// - `interrupts` 是**完整**的固件中断资源列表（长度即真相）：一台设备可以有多条
///   中断（多个资源 / 不同控制器），每条资源的 `line` 是 Core 后端支持的逻辑
///   外部 IRQ 号（未绑定 = `None`）。
/// - `compatibles` 是**完整**的 compatible 列表（数量与长度都不截断）。
#[derive(Clone)]
pub struct DeviceDescriptor {
    pub spaces: Box<[IoSpace]>,
    pub interrupts: Box<[InterruptResource]>,
    pub compatibles: Box<[Box<str>]>,
}

impl DeviceDescriptor {
    /// 空描述符（无空间 / 无中断 / 无 compatible；三个 boxed slice 都是空的）。
    pub fn empty() -> Self {
        Self {
            spaces: Box::new([]),
            interrupts: Box::new([]),
            compatibles: Box::new([]),
        }
    }

    /// 该描述符是否声明了 `compatible`（任一串命中即计一次）。
    /// 纯谓词：不看可用性 / claim 状态，也不触碰设备；遍历**全部** compatible。
    pub fn matches(&self, compatible: &[u8]) -> bool {
        self.compatibles.iter().any(|c| c.as_bytes() == compatible)
    }
}

/// 设备身份（identity，**不是 authority**）：命名 `MachineInfo.devices` 中的一条记录。
///
/// - 可自由复制、比较、透传；Core 把它解析回那条设备记录。
/// - **不是** Handle、不可撤销、不携带权限；零可以是合法值。
/// - 表示形式是实现细节：消费者**不得**把它解释成地址、IRQ 号、过滤后的序号，
///   或跨启动持久的身份。它只在一个已提交 `MachineInfo` 的生命周期内有意义。
/// - 全宽 `u32`：设备表长度由 `core::init` 校验能被 `u32` 索引（无 u8 收窄）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceId(u32);

impl DeviceId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// 纯设备发现失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceLookupError {
    /// 机器信息尚未提交（正常组件运行期不可达）。
    NoMachineInfo,
    /// `ordinal` 超出匹配数量（包括完全没有匹配）——枚举的**唯一**终止信号。
    NoSuchOrdinal,
}

/// 纯设备发现：取第 `ordinal` 个匹配描述符；空 compatible 枚举所有设备。
///
/// - **不分配、不预留、不触碰任何设备寄存器、不读取 claim 状态**；
/// - 一条描述符匹配**任意** compatible 串即计一次；
/// - **枚举整张设备表**（长度即真相，无 count 截断）；
/// - 枚举**包含已认领设备**，且顺序只取决于已提交的 `MachineInfo`——因此跨
///   claim/release 稳定；
/// - `ordinal >= 匹配数` → [`DeviceLookupError::NoSuchOrdinal`]。
///
/// 身份不是权限：调用方只能拿这个 ID 去 [`crate::resource::device::claim`]
/// 请求该**确切设备**的 authority；ID 本身不授予任何东西。
pub fn nth_compatible(compatible: &[u8], ordinal: u32) -> Result<DeviceId, DeviceLookupError> {
    nth_compatible_in(committed(), compatible, ordinal)
}

/// [`nth_compatible`] 的纯逻辑核心：把已提交快照显式传入，让 `NoMachineInfo`
/// 路径可以确定性 host 测试（进程全局快照无法在测试间回退）。
fn nth_compatible_in(
    info: Option<&MachineInfo>,
    compatible: &[u8],
    ordinal: u32,
) -> Result<DeviceId, DeviceLookupError> {
    let Some(info) = info else {
        return Err(DeviceLookupError::NoMachineInfo);
    };
    let mut seen = 0u32;
    for (index, device) in info.devices.iter().enumerate() {
        if !compatible.is_empty() && !device.matches(compatible) {
            continue;
        }
        if seen == ordinal {
            // 设备表长度已由 `commit` 校验可被 u32 索引；不可能失败的转换只做防御。
            let Ok(raw) = u32::try_from(index) else {
                break;
            };
            return Ok(DeviceId::from_raw(raw));
        }
        seen += 1;
    }
    Err(DeviceLookupError::NoSuchOrdinal)
}

impl core::fmt::Debug for DeviceDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 全部窗口按固件顺序（`spaces[0]` 是主窗口）；长度即真相，无计数截断。
        write!(f, "DeviceDescriptor {{ spaces: [")?;
        for (index, space) in self.spaces.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            let (label, base, size) = match space {
                IoSpace::Mmio { base, size } => ("mmio", base, size),
                IoSpace::Pio { base, size } => ("pio", base, size),
            };
            write!(f, "{label} {base:#x}+")?;
            write_size(f, *size)?;
        }
        write!(f, "], interrupts: {:?}, compatibles: ", self.interrupts)?;
        f.debug_list().entries(self.compatibles.iter()).finish()?;
        f.write_str(" }")
    }
}

/// 机器真相快照（owned、不可变）：bootstrap 用 `Vec` 构造 →
/// `core::init` 校验后 [`commit`] 一次 → 全系统经 [`committed`] 借用。
///
/// 长度即真相（没有 `cpu_count` / `mem_count` / `dev_count`）：
/// - `cpu_info`：已承认的逻辑 CPU（BSP 恒为下标 0；`1..=MAX_CPUS`）；
/// - `memory_regions`：发现的 RAM inventory（可空？不——`core::init` 拒绝空表）；
/// - `devices`：发现的设备记录（可为空；`DeviceId` 全宽索引）。
pub struct MachineInfo {
    /// BSP 的**硬件**身份（不是逻辑 `CpuId`；逻辑 id 由 Core 按下标赋）。
    pub boot_hardware_id: HardwareCpuId,
    /// 平台 timebase 频率（Hz）：`Some(non-zero)` = 已发现的真实速率，
    /// `None` = **未知**（不是 0 约定，也不伪造常量）。需要速率换算的消费者
    /// （`timer::init_preempt` / idle 换算 / 组件导出）各自显式处理未知：
    /// 抢占 fail-closed，idle 回退轮询。
    pub timebase_frequency: Option<NonZeroU64>,
    /// 保留的原始固件描述源（归一化视图之外；见 [`FirmwareInfo`]）。
    pub firmware: FirmwareInfo,
    /// 已承认的逻辑 CPU：BSP 在下标 0，硬件身份唯一。
    pub cpu_info: Box<[CpuInfo]>,
    /// 发现的 RAM inventory（不是空闲内存、不是 arena）。
    pub memory_regions: Box<[MemoryRegion]>,
    pub devices: Box<[DeviceDescriptor]>,
}

impl core::fmt::Debug for MachineInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MachineInfo")
            .field("boot_hardware_id", &self.boot_hardware_id)
            .field("timebase_frequency", &self.timebase_frequency)
            .field("firmware", &self.firmware)
            .field("cpu_info", &self.cpu_info)
            .field("memory_regions", &self.memory_regions)
            .field("devices", &self.devices)
            .finish()
    }
}

/// 已提交的机器真相：一次性发布，**不可替换**。
static COMMITTED: spin::Once<MachineInfo> = spin::Once::new();

/// `commit` 的互斥门：让"第二次发布"在并发下也确定性地被拒绝。
static COMMIT_GATE: spin::Mutex<()> = spin::Mutex::new(());

/// 一次性发布机器真相（`core::init` 校验通过后调用一次）：唯一真相存放点。
///
/// **拒绝第二次发布**：这是 BSP 启动期操作，不是运行时替换 / reset API。
/// 发布后任何失败都是终止启动（没有 retry）。
pub(crate) fn commit(info: MachineInfo) -> Result<&'static MachineInfo, &'static str> {
    let _gate = COMMIT_GATE.lock();
    if COMMITTED.get().is_some() {
        return Err("machine info already committed");
    }
    COMMITTED.call_once(|| info);
    COMMITTED.get().ok_or("machine info commit failed")
}

/// 读已提交的机器真相（monitor / export / resource 共用同一份不可变快照）。
pub fn committed() -> Option<&'static MachineInfo> {
    #[cfg(test)]
    if let Some(info) = test_support::installed() {
        return Some(info);
    }
    COMMITTED.get()
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{
        CpuInfo, DeviceDescriptor, FirmwareInfo, HardwareCpuId, MachineInfo, MemoryRegion,
        NonZeroU64,
    };
    use crate::test_support::{Rank, TestLock};
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    /// 串行化「安装/替换全局机器快照」的测试：`committed()` 的读路径是进程全局，
    /// 并行测试各自安装 fixture 会互相覆盖。测试很短，用自旋锁串起来即可。
    ///
    /// rank = MACHINE（规范顺序 `SCHED → LOAD → IRQ → TIMER → BOUNDARY → MACHINE → MEMORY → TRACE`；见
    /// [`crate::test_support`]）。
    pub(crate) static GUARD: TestLock = TestLock::new(Rank::Machine);

    /// 测试专用覆盖槽：`COMMITTED: Once` 不能回退，host 用例把各自的
    /// **泄漏的不可变 fixture** 装在这里，[`super::committed`] 优先返回它。
    ///
    /// **仅测试**：生产没有 reset / 替换 API——`commit` 依旧只发布一次。
    static OVERRIDE: spin::Mutex<Option<&'static MachineInfo>> = spin::Mutex::new(None);

    /// 构造 owned fixture（host 测试专用；production 由 boot 从 `Vec` 构造）。
    /// 无保留固件源（[`FirmwareInfo::Static`]）；需要固件真相的用例走
    /// [`snapshot_with_firmware`]。
    pub(crate) fn snapshot(
        boot_hardware_id: HardwareCpuId,
        timebase_frequency: Option<NonZeroU64>,
        cpu_info: Vec<CpuInfo>,
        memory_regions: Vec<MemoryRegion>,
        devices: Vec<DeviceDescriptor>,
    ) -> MachineInfo {
        snapshot_with_firmware(
            boot_hardware_id,
            timebase_frequency,
            FirmwareInfo::Static,
            cpu_info,
            memory_regions,
            devices,
        )
    }

    /// 带保留固件源的 fixture（其余与 [`snapshot`] 相同）。
    pub(crate) fn snapshot_with_firmware(
        boot_hardware_id: HardwareCpuId,
        timebase_frequency: Option<NonZeroU64>,
        firmware: FirmwareInfo,
        cpu_info: Vec<CpuInfo>,
        memory_regions: Vec<MemoryRegion>,
        devices: Vec<DeviceDescriptor>,
    ) -> MachineInfo {
        MachineInfo {
            boot_hardware_id,
            timebase_frequency,
            firmware,
            cpu_info: cpu_info.into_boxed_slice(),
            memory_regions: memory_regions.into_boxed_slice(),
            devices: devices.into_boxed_slice(),
        }
    }

    /// 安装 fixture（调用方必须持有 [`GUARD`]）：泄漏成 `&'static` 并覆盖全局读路径。
    pub(crate) fn install(info: MachineInfo) -> &'static MachineInfo {
        let leaked: &'static MachineInfo = Box::leak(Box::new(info));
        *OVERRIDE.lock() = Some(leaked);
        leaked
    }

    /// 当前已安装的 fixture（未安装 = `None`）。
    pub(crate) fn installed() -> Option<&'static MachineInfo> {
        *OVERRIDE.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::num::NonZeroU64;

    /// FDT magic（测试哨兵：模拟保留的 FDT header 可重读）。
    const FDT_MAGIC: u32 = 0xd00d_feed;

    /// 一条已绑定的 ISA 中断资源（测试便利构造）。
    fn irq_resource(line: u32) -> InterruptResource {
        InterruptResource {
            specifier: InterruptSpecifier::Isa { line: line as u8 },
            line: Some(line),
        }
    }

    /// 一条未绑定的 FDT 中断资源（测试便利构造）。
    fn fdt_resource(controller: u32, cells: &[u32]) -> InterruptResource {
        InterruptResource {
            specifier: InterruptSpecifier::Fdt {
                controller,
                cells: cells.to_vec().into_boxed_slice(),
            },
            line: None,
        }
    }

    /// 构造一个只有 compatible + interrupts 的设备（无空间窗口；需要窗口的
    /// 用例自行填 `spaces`）。
    fn device(compatibles: &[&str], interrupts: Vec<InterruptResource>) -> DeviceDescriptor {
        let mut d = DeviceDescriptor::empty();
        d.compatibles = compatibles.iter().map(|c| Box::<str>::from(*c)).collect();
        d.interrupts = interrupts.into_boxed_slice();
        d
    }

    fn cpu0() -> CpuInfo {
        CpuInfo {
            boot_cpu: true,
            hardware_id: HardwareCpuId::from_raw(0),
        }
    }

    fn ram() -> MemoryRegion {
        MemoryRegion {
            base: 0x8000_0000,
            size: 0x1000_0000,
        }
    }

    /// 一份单 CPU / 单 RAM 区 / 指定设备的 fixture。
    fn fixture(devices: Vec<DeviceDescriptor>) -> MachineInfo {
        test_support::snapshot(
            HardwareCpuId::from_raw(0),
            NonZeroU64::new(10_000_000),
            vec![cpu0()],
            vec![ram()],
            devices,
        )
    }

    /// 枚举纯逻辑：ordinal 是 zero-based 匹配序号，越界（含无匹配）→ NoSuchOrdinal；
    /// 一条描述符命中多个 compatible 只计一次；顺序就是设备表顺序（与 claim 无关）。
    #[test]
    fn nth_compatible_counts_once_per_descriptor_and_terminates_with_no_such_ordinal() {
        // devices[0] 同时声明两个 compatible —— 匹配任一个都只算一台设备。
        let info = fixture(vec![
            device(&["virtio,mmio", "legacy,mmio"], vec![irq_resource(1)]),
            device(&["ns16550a"], vec![irq_resource(10)]),
            device(&["virtio,mmio"], vec![irq_resource(2)]),
        ]);

        assert_eq!(
            nth_compatible_in(Some(&info), b"virtio,mmio", 0),
            Ok(DeviceId::from_raw(0))
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"virtio,mmio", 1),
            Ok(DeviceId::from_raw(2))
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"virtio,mmio", 2),
            Err(DeviceLookupError::NoSuchOrdinal)
        );
        // 另一个 compatible 命中同一描述符，仍只是 ordinal 0。
        assert_eq!(
            nth_compatible_in(Some(&info), b"legacy,mmio", 0),
            Ok(DeviceId::from_raw(0))
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"legacy,mmio", 1),
            Err(DeviceLookupError::NoSuchOrdinal)
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"ns16550a", 0),
            Ok(DeviceId::from_raw(1))
        );
        // 完全无匹配 → 同样是 NoSuchOrdinal（枚举的唯一终止信号）。
        assert_eq!(
            nth_compatible_in(Some(&info), b"nope,device", 0),
            Err(DeviceLookupError::NoSuchOrdinal)
        );
    }

    /// 机器信息未提交 → NoMachineInfo（导出层翻译成 `-ENODEV`）。
    #[test]
    fn nth_compatible_without_machine_info_is_unavailable() {
        assert_eq!(
            nth_compatible_in(None, b"virtio,mmio", 0),
            Err(DeviceLookupError::NoMachineInfo)
        );
    }

    #[test]
    fn unfiltered_discovery_includes_devices_without_compatible() {
        let info = fixture(vec![device(&[], vec![]), device(&["virtio,mmio"], vec![])]);
        assert_eq!(
            nth_compatible_in(Some(&info), b"", 0),
            Ok(DeviceId::from_raw(0))
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"", 1),
            Ok(DeviceId::from_raw(1))
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"", 2),
            Err(DeviceLookupError::NoSuchOrdinal)
        );
    }

    /// 全宽设备身份：`DeviceId ≥ 256` 的设备**参与枚举**——旧的 u8 收窄 / 256
    /// 上限已删除，长度即真相。
    #[test]
    fn nth_compatible_reaches_devices_beyond_a_byte() {
        let mut devices = vec![DeviceDescriptor::empty(); 300];
        devices[260] = device(&["far,device"], vec![irq_resource(5)]);
        let info = fixture(devices);

        assert_eq!(
            nth_compatible_in(Some(&info), b"far,device", 0),
            Ok(DeviceId::from_raw(260)),
            "第 260 台设备必须可枚举"
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"far,device", 1),
            Err(DeviceLookupError::NoSuchOrdinal)
        );
    }

    /// 快照保留 > 26 台设备与 > 16 个 RAM 区（旧定长数组的 discover 上限已删除）：
    /// 长度即真相，枚举走完整切片。
    #[test]
    fn snapshot_retains_more_than_sixteen_regions_and_twenty_six_devices() {
        let regions = (0..20)
            .map(|i| MemoryRegion {
                base: 0x8000_0000 + i * 0x1000,
                size: 0x1000,
            })
            .collect::<Vec<_>>();
        let devices = (0..40)
            .map(|_| device(&["many,device"], vec![irq_resource(3)]))
            .collect::<Vec<_>>();
        let info = test_support::snapshot(
            HardwareCpuId::from_raw(0),
            NonZeroU64::new(10_000_000),
            vec![cpu0()],
            regions,
            devices,
        );

        assert_eq!(info.memory_regions.len(), 20);
        assert_eq!(info.devices.len(), 40);
        assert_eq!(
            nth_compatible_in(Some(&info), b"many,device", 39),
            Ok(DeviceId::from_raw(39)),
            "第 39 台设备（>26）必须可枚举"
        );
    }

    /// 发布一次性：第二次 `commit` 被拒绝，已提交快照不变；这是 BSP 启动期操作，
    /// 没有运行时替换 / reset API。
    #[test]
    fn commit_publishes_once_and_rejects_a_second_call() {
        let _guard = test_support::GUARD.lock();
        let first = commit(fixture(vec![])).expect("first commit must publish");
        assert_eq!(first.devices.len(), 0);

        let second = commit(fixture(vec![device(&["late,device"], vec![])]));
        assert_eq!(second.err(), Some("machine info already committed"));
        assert_eq!(
            COMMITTED.get().expect("still committed").devices.len(),
            0,
            "第二次发布不得替换已提交快照"
        );
    }

    /// `write_size`（经 `MemoryRegion` Debug 观察）：**只有**整数 GiB/MiB/KiB 使用
    /// 单位；其余一律按字节显示，绝不做会撒谎的四舍五入。
    #[test]
    fn memory_region_debug_formats_size_units_exactly() {
        let region = |size: usize| {
            alloc::format!(
                "{:?}",
                MemoryRegion {
                    base: 0x8000_0000,
                    size
                }
            )
        };

        // 整数单位（含边界 1、倍数、0）。
        assert_eq!(region(0), "MemoryRegion { base: 0x80000000, size: 0 B }");
        assert_eq!(
            region(1 << 10),
            "MemoryRegion { base: 0x80000000, size: 1 KiB }"
        );
        assert_eq!(
            region(1 << 20),
            "MemoryRegion { base: 0x80000000, size: 1 MiB }"
        );
        assert_eq!(
            region(256 << 20),
            "MemoryRegion { base: 0x80000000, size: 256 MiB }"
        );
        assert_eq!(
            region(1 << 30),
            "MemoryRegion { base: 0x80000000, size: 1 GiB }"
        );
        assert_eq!(
            region(2 << 30),
            "MemoryRegion { base: 0x80000000, size: 2 GiB }"
        );

        // 非整数倍：>= KiB 但不是 KiB 整数倍 → 按字节；>= MiB 但非 MiB 整数倍
        // 也会一路落到字节（不显示 "1 MiB + 1 B" 这种近似）。
        assert_eq!(
            region(1500),
            "MemoryRegion { base: 0x80000000, size: 1500 B }"
        );
        assert_eq!(
            region((1 << 10) + 1),
            "MemoryRegion { base: 0x80000000, size: 1025 B }"
        );
        assert_eq!(
            region((1 << 20) + 1),
            "MemoryRegion { base: 0x80000000, size: 1048577 B }"
        );
    }

    /// 4e 形状：一条设备可以有多条 MMIO 窗口、多条中断资源、任意数量与长度的
    /// compatible——全部按固件顺序保留（无 4 槽 / 32B 截断）。
    #[test]
    fn device_retains_all_windows_interrupts_and_compatibles() {
        // 超过旧容量（4）的 compatible 数量；其中一个超过旧 32B 上限。
        const LONG: &str = "vendor,extremely-long-compatible-string-beyond-the-old-32-byte-limit";
        let mut d = device(
            &[
                "virtio,mmio",
                "vendor,secondary",
                "vendor,tertiary",
                "vendor,quaternary",
                "vendor,quinary",
                LONG,
            ],
            vec![
                fdt_resource(3, &[10, 1]),
                irq_resource(7),
                fdt_resource(4, &[0x2a]),
            ],
        );
        d.spaces = vec![
            IoSpace::Mmio {
                base: 0x1000_0000,
                size: 0x1000,
            },
            IoSpace::Mmio {
                base: 0x1000_1000,
                size: 0x2000,
            },
            IoSpace::Mmio {
                base: 0x2000_0000,
                size: 0x4000,
            },
        ]
        .into_boxed_slice();

        // 长度即真相：全部保留、按固件顺序（spaces[0] 是主窗口）。
        assert_eq!(d.spaces.len(), 3);
        assert_eq!(
            d.spaces[0],
            IoSpace::Mmio {
                base: 0x1000_0000,
                size: 0x1000
            }
        );
        assert_eq!(
            d.spaces[2],
            IoSpace::Mmio {
                base: 0x2000_0000,
                size: 0x4000
            }
        );
        assert_eq!(d.interrupts.len(), 3, "多条中断资源全部保留");
        assert_eq!(d.compatibles.len(), 6, "超过旧 4 槽的 compatible 全部保留");
        assert_eq!(&*d.compatibles[5], LONG);
        assert!(d.compatibles[5].len() > 32, "compatible 长度不截断");

        // matches 遍历整张列表（含最后一个超长串）；前缀不算命中。
        assert!(d.matches(b"virtio,mmio"));
        assert!(d.matches(b"vendor,quinary"));
        assert!(d.matches(LONG.as_bytes()));
        assert!(!d.matches(b"vendor,missing"));
        assert!(!d.matches(&LONG.as_bytes()[..31]), "前缀不是命中");

        // 枚举同样能看到任意一个声明的 compatible。
        let info = fixture(vec![d]);
        assert_eq!(
            nth_compatible_in(Some(&info), LONG.as_bytes(), 0),
            Ok(DeviceId::from_raw(0))
        );
    }

    /// `DeviceDescriptor::matches`：空描述符不匹配任何 compatible；声明的**任一**
    /// 串命中即为真（整张列表参与，无 count / 容量截断）。
    #[test]
    fn device_matches_any_declared_compatible() {
        // 零个 compatible：无论问什么都是 false。
        assert!(!DeviceDescriptor::empty().matches(b"virtio,mmio"));
        assert!(!DeviceDescriptor::empty().matches(b""));

        // 声明两个 compatible：命中任一为真，无关串为假（前缀也不算命中）。
        let d = device(&["virtio,mmio", "legacy,mmio"], vec![irq_resource(1)]);
        assert!(d.matches(b"virtio,mmio"), "第一个声明的 compatible");
        assert!(d.matches(b"legacy,mmio"), "第二个声明的 compatible");
        assert!(!d.matches(b"ns16550a"), "未声明的 compatible");
        assert!(!d.matches(b"virtio,mmi"), "前缀不是命中");
        assert!(!d.matches(b""), "空查询不命中非空串");

        // 第 5 个兼容串同样参与匹配（旧的 4 槽截断已删除）。
        let many = device(&["a", "b", "c", "d", "hidden,mmio"], vec![]);
        assert!(
            many.matches(b"hidden,mmio"),
            "超出旧 4 槽的声明必须参与匹配"
        );
    }

    /// DeviceDescriptor Debug：全部窗口按固件顺序（MMIO/PIO 标签 + `write_size`
    /// 单位）、IRQ Some/None 与完整 compatible 列表；不 panic 且含预期子串。
    #[test]
    fn device_descriptor_debug_reports_spaces_irq_and_compatibles() {
        // Given: 三个窗口（两 MMIO + 一 PIO）、IRQ、两个 compatible。
        let mut d = device(&["virtio,mmio", "legacy,mmio"], vec![irq_resource(7)]);
        d.spaces = vec![
            IoSpace::Mmio {
                base: 0x1000,
                size: 0x1000,
            },
            IoSpace::Pio {
                base: 0x3f8,
                size: 8,
            },
            IoSpace::Mmio {
                base: 0x2000,
                size: 0x2000,
            },
        ]
        .into_boxed_slice();

        // When: 格式化。
        let text = alloc::format!("{d:?}");

        // Then: 全部窗口/大小/IRQ/compatible 都可读（列表完整，无计数截断）。
        assert!(text.contains("DeviceDescriptor"), "{text}");
        assert!(
            text.contains("spaces: [mmio 0x1000+4 KiB, pio 0x3f8+8 B, mmio 0x2000+8 KiB]"),
            "{text}"
        );
        assert!(
            text.contains(
                "interrupts: [InterruptResource { specifier: Isa { line: 7 }, line: Some(7) }]"
            ),
            "{text}"
        );
        assert!(
            text.contains("[\"virtio,mmio\", \"legacy,mmio\"]"),
            "{text}"
        );

        // 空描述符：三个列表都为空。
        let text = alloc::format!("{:?}", DeviceDescriptor::empty());
        assert!(text.contains("spaces: []"), "{text}");
        assert!(text.contains("interrupts: []"), "{text}");
        assert!(text.contains("compatibles: []"), "{text}");
    }

    /// 一条 FDT 中断资源：完整 cells 被保留，`line` 与 specifier 分开显示
    /// （未绑定 = `None`，绝不把 cell 当逻辑 IRQ 号）。
    #[test]
    fn interrupt_resource_debug_distinguishes_specifier_from_line() {
        let resource = fdt_resource(3, &[0x0a, 1]);
        let text = alloc::format!("{resource:?}");
        assert!(text.contains("controller: 3"), "{text}");
        assert!(text.contains("cells: [10, 1]"), "{text}");
        assert!(text.contains("line: None"), "{text}");

        let bound = InterruptResource {
            specifier: InterruptSpecifier::Fdt {
                controller: 3,
                cells: alloc::vec![10].into_boxed_slice(),
            },
            line: Some(10),
        };
        let text = alloc::format!("{bound:?}");
        assert!(text.contains("line: Some(10)"), "{text}");
    }

    /// MachineInfo Debug：整张表可见（长度即真相，没有 count 字段），空设备表
    /// 打印空列表。
    #[test]
    fn machine_info_debug_prints_full_tables_and_no_counts() {
        // Given: 1 CPU / 1 memory region / 3 devices。
        let info = fixture(vec![
            device(&["virtio,mmio"], vec![irq_resource(1)]),
            device(&["ns16550a"], vec![irq_resource(10)]),
            device(&["riscv,clint0"], vec![]),
        ]);

        // When: 格式化（完整切片，不越界）。
        let text = alloc::format!("{info:?}");

        // Then: 身份 + 表内容可见；计数字段已不存在。
        assert!(text.contains("MachineInfo"), "{text}");
        assert!(
            text.contains("boot_hardware_id: HardwareCpuId(0)"),
            "{text}"
        );
        assert!(text.contains("memory_regions"), "{text}");
        assert!(text.contains("256 MiB"), "{text}");
        assert!(text.contains("virtio,mmio"), "{text}");
        assert!(text.contains("riscv,clint0"), "{text}");
        assert!(text.contains("firmware: Static"), "{text}");
        assert!(!text.contains("cpu_count"), "{text}");
        assert!(!text.contains("mem_count"), "{text}");
        assert!(!text.contains("dev_count"), "{text}");

        // 空设备表：正常 Debug，仍然不 panic。
        let empty = fixture(vec![]);
        let text = alloc::format!("{empty:?}");
        assert!(text.contains("devices: []"), "{text}");
    }

    /// 一份带指定保留固件源的单 CPU / 单 RAM 区 fixture。
    fn fixture_with_firmware(firmware: FirmwareInfo) -> MachineInfo {
        test_support::snapshot_with_firmware(
            HardwareCpuId::from_raw(0),
            NonZeroU64::new(10_000_000),
            firmware,
            vec![cpu0()],
            vec![ram()],
            vec![],
        )
    }

    /// 保留的固件源是**位置真相**：快照发布后大量分配，`committed()` 仍报告同
    /// 一变体/取值，且来源地址上的字节仍可按原样重读（模拟未来消费者）。
    #[test]
    fn firmware_info_survives_heavy_allocation_and_source_stays_readable() {
        let _guard = test_support::GUARD.lock();

        // Given: boot 保留的 FDT 字节（泄漏缓冲模拟固件源）与带 FirmwareInfo 的已安装快照。
        let mut source = vec![0xa5u8; 512];
        source[..4].copy_from_slice(&FDT_MAGIC.to_be_bytes());
        let source: &'static mut [u8] = Box::leak(source.into_boxed_slice());
        let phys = source.as_ptr() as usize;
        let size = source.len();
        let _installed =
            test_support::install(fixture_with_firmware(FirmwareInfo::Fdt { phys, size }));

        // When: 大量分配 / 释放（搬动堆内其它对象，不得动到保留源）。
        let mut pressure: Vec<Vec<u8>> = Vec::new();
        for _ in 0..64 {
            pressure.push(vec![0xAAu8; 256 * 1024]);
        }
        pressure.shrink_to_fit();
        drop(pressure);

        // Then: 已提交快照仍报告同一来源，重读的字节未变。
        let committed = committed().expect("fixture must be installed");
        assert!(
            matches!(
                committed.firmware,
                FirmwareInfo::Fdt { phys: p, size: s } if p == phys && s == size
            ),
            "firmware 来源不得在分配后漂移: {:?}",
            committed.firmware
        );
        // SAFETY: `source` 是本测试泄漏的缓冲区，`phys` 就是它的地址。
        let reread = unsafe { core::slice::from_raw_parts(phys as *const u8, size) };
        assert_eq!(reread, &source[..], "保留源必须仍可读且内容未变");
        assert_eq!(
            u32::from_be_bytes(reread[..4].try_into().unwrap()),
            FDT_MAGIC
        );
    }

    /// `Static` / `Acpi` 变体经快照往返保持不变，Debug 输出可读。
    #[test]
    fn firmware_info_variants_round_trip_and_are_visible_in_debug() {
        // Static：默认 fixture（无保留固件源）。
        let static_info = fixture(vec![]);
        assert!(matches!(static_info.firmware, FirmwareInfo::Static));
        let text = alloc::format!("{:?}", static_info.firmware);
        assert_eq!(text, "Static");

        // Acpi：安装后 `committed()` 报告同一 rsdp；Debug 保留字段名与值。
        let _guard = test_support::GUARD.lock();
        let _installed =
            test_support::install(fixture_with_firmware(FirmwareInfo::Acpi { rsdp: 0xf_0000 }));
        let committed = committed().expect("fixture must be installed");
        assert!(matches!(
            committed.firmware,
            FirmwareInfo::Acpi { rsdp: 0xf_0000 }
        ));
        let text = alloc::format!("{:?}", committed.firmware);
        assert_eq!(text, "Acpi { rsdp: 983040 }", "{text}");
    }
}
