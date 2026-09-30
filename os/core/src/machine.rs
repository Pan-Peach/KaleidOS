//! 归一化机器信息（MachineInfo）：bootstrap 发现 → core::init 消费。
//! 单镜像内函数调用交接，用 Rust 类型即可（无需跨 binary POD/协议）。
//! **owned 值类型**：不借用 DTB —— bootstrap 把需要的字符串/数值复制进
//! 定长数组后，DTB 即可丢弃；core::init 消费的是 Core 自己的真相。
//! 字段语义：Core Resource Truth 的"提案"，由 core::init 校验后提交。

use core::fmt::Debug;

/// 兼容性字符串块：内嵌定长（FDT compatible 一般 ≤ 32B），值类型可 Copy。
#[derive(Clone, Copy)]
pub struct CompatStr {
    len: u8,
    bytes: [u8; 32],
}

impl CompatStr {
    pub const fn empty() -> Self {
        Self {
            len: 0,
            bytes: [0; 32],
        }
    }

    /// 从字节切片复制（截断至容量）。
    pub fn from_bytes(src: &[u8]) -> Self {
        let mut s = Self::empty();
        let n = src.len().min(s.bytes.len());
        s.bytes[..n].copy_from_slice(&src[..n]);
        s.len = n as u8;
        s
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

impl core::fmt::Debug for CompatStr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

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

/// Discovery 能承载的最大 CPU 数（`MachineInfo.cpu_info` 的容量）。
///
/// 这是**编译期容量**（定长数组大小），不是运行时真值：真实 CPU 数一律由
/// `MachineInfo.cpu_count`（bootstrap 从 FDT/ACPI 发现后填写）决定。SMP 的
/// `CpuMask` / per-CPU 索引以本常量为上界；Core 只使用 `[..cpu_count]` 前缀。
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

/// 设备的一个空间条目：MMIO 窗口或 PIO 窗口，互斥由类型保证。
/// x86 特有 PIO（RISC-V/ARM 只有 MMIO）；一个设备可占多条目（如 PCI 双 BAR）。
/// 预留：X86_64 arch 发 Pio 条目，Core 的 Handle 机制据此选择访问原语。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoSpace {
    Mmio { base: usize, size: usize },
    Pio { base: usize, size: usize },
}

#[derive(Clone, Copy)]
pub struct DeviceDescriptor {
    pub space: IoSpace,
    pub irq: Option<u32>,
    pub compatibles: [CompatStr; 4],
    pub compat_count: u8,
}

impl DeviceDescriptor {
    pub const fn empty() -> Self {
        Self {
            space: IoSpace::Mmio { base: 0, size: 0 },
            irq: None,
            compatibles: [CompatStr::empty(); 4],
            compat_count: 0,
        }
    }

    /// 该描述符是否声明了 `compatible`（任一串命中即计一次）。
    /// 纯谓词：不看可用性 / claim 状态，也不触碰设备。
    pub fn matches(&self, compatible: &[u8]) -> bool {
        self.compatibles[..self.compat_count as usize]
            .iter()
            .any(|c| c.as_str().as_bytes() == compatible)
    }
}

/// 设备身份（identity，**不是 authority**）：命名 `MachineInfo.devices` 中的一条记录。
///
/// - 可自由复制、比较、透传；Core 把它解析回那条设备记录。
/// - **不是** Handle、不可撤销、不携带权限；零可以是合法值。
/// - 表示形式是实现细节：消费者**不得**把它解释成地址、IRQ 号、过滤后的序号，
///   或跨启动持久的身份。它只在一个已提交 `MachineInfo` 的生命周期内有意义。
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

/// 纯设备发现：按 compatible 取第 `ordinal` 个匹配描述符（zero-based）。
///
/// - **不分配、不预留、不触碰任何设备寄存器、不读取 claim 状态**；
/// - 一条描述符匹配**任意** compatible 串即计一次；
/// - 枚举**包含已认领设备**，且顺序只取决于已提交的 `MachineInfo`——因此跨
///   claim/release 稳定；
/// - `ordinal >= 匹配数` → [`DeviceLookupError::NoSuchOrdinal`]。
///
/// 身份不是权限：调用方只能拿这个 ID 去 [`crate::resource::device::claim`]
/// 请求该**确切设备**的 authority；ID 本身不授予任何东西。
pub fn nth_compatible(compatible: &[u8], ordinal: u32) -> Result<DeviceId, DeviceLookupError> {
    nth_compatible_in(committed().as_ref(), compatible, ordinal)
}

/// [`nth_compatible`] 的纯逻辑核心：把已提交快照显式传入，让 `NoMachineInfo`
/// 路径可以确定性 host 测试（进程全局 `COMMITTED` 无法在测试间回退）。
fn nth_compatible_in(
    info: Option<&MachineInfo>,
    compatible: &[u8],
    ordinal: u32,
) -> Result<DeviceId, DeviceLookupError> {
    let Some(info) = info else {
        return Err(DeviceLookupError::NoMachineInfo);
    };
    let mut seen = 0u32;
    for (index, device) in info.devices[..info.dev_count.min(info.devices.len())]
        .iter()
        .enumerate()
    {
        if !device.matches(compatible) {
            continue;
        }
        if seen == ordinal {
            return Ok(DeviceId(index as u32));
        }
        seen += 1;
    }
    Err(DeviceLookupError::NoSuchOrdinal)
}

impl core::fmt::Debug for DeviceDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (space, base, size) = match self.space {
            IoSpace::Mmio { base, size } => ("mmio", base, size),
            IoSpace::Pio { base, size } => ("pio", base, size),
        };
        write!(f, "DeviceDescriptor {{ {space}: {base:#x}, size: ")?;
        write_size(f, size)?;
        match self.irq {
            Some(irq) => write!(f, ", irq: {irq}, compatibles: ")?,
            None => write!(f, ", irq: None, compatibles: ")?,
        }
        f.write_str("[")?;
        for (i, c) in self.compatibles[..self.compat_count as usize]
            .iter()
            .enumerate()
        {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{c:?}")?;
        }
        f.write_str("] }")
    }
}

/// 定长机器信息（owned）：bootstrap 填满 → core::init 校验提交。
/// 所有字段都是值，无借用 → DTB 可丢，MachineInfo 可自由传递/持久化。
#[derive(Clone, Copy)]
pub struct MachineInfo {
    /// BSP 的**硬件**身份（不是逻辑 `CpuId`；逻辑 id 由 Core 按下标赋）。
    pub boot_hardware_id: HardwareCpuId,
    pub timebase_frequency: u64,
    pub cpu_count: usize,
    pub cpu_info: [CpuInfo; MAX_CPUS],
    pub mem_count: usize,
    pub memory_regions: [MemoryRegion; 16],
    pub dev_count: usize,
    pub devices: [DeviceDescriptor; 26],
}

impl core::fmt::Debug for MachineInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MachineInfo")
            .field("boot_hardware_id", &self.boot_hardware_id)
            .field("timebase_frequency", &self.timebase_frequency)
            .field("cpu_count", &self.cpu_count)
            .field("cpu_info", &&self.cpu_info[..self.cpu_count])
            .field("mem_count", &self.mem_count)
            .field("memory_regions", &&self.memory_regions[..self.mem_count])
            .field("dev_count", &self.dev_count)
            .field("devices", &&self.devices[..self.dev_count])
            .finish()
    }
}

static COMMITTED: spin::Mutex<Option<MachineInfo>> = spin::Mutex::new(None);

/// 提交 MachineInfo（core::init 校验通过后调用一次）：唯一真相存放点。
pub fn commit(info: MachineInfo) {
    *COMMITTED.lock() = Some(info);
}

/// 读已提交的 MachineInfo（monitor / export table 共用同一份快照）。
pub fn committed() -> Option<MachineInfo> {
    *COMMITTED.lock()
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::test_support::{Rank, TestLock};

    /// 串行化「提交全局 MachineInfo」的测试：`COMMITTED` 是进程全局，并行测试
    /// 各自 commit 一份会互相覆盖。测试很短，用自旋锁串起来即可。
    ///
    /// rank = MACHINE（规范顺序 `SCHED → LOAD → IRQ → TIMER → BOUNDARY → MACHINE → MEMORY → TRACE`；见
    /// [`crate::test_support`]）。
    pub(crate) static GUARD: TestLock = TestLock::new(Rank::Machine);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(compatibles: &[&[u8]], irq: Option<u32>) -> DeviceDescriptor {
        let mut d = DeviceDescriptor::empty();
        for (slot, c) in d.compatibles.iter_mut().zip(compatibles) {
            *slot = CompatStr::from_bytes(c);
        }
        d.compat_count = compatibles.len() as u8;
        d.irq = irq;
        d
    }

    fn info(devices: [DeviceDescriptor; 26], dev_count: usize) -> MachineInfo {
        MachineInfo {
            boot_hardware_id: HardwareCpuId::from_raw(0),
            timebase_frequency: 10_000_000,
            cpu_count: 1,
            cpu_info: [CpuInfo {
                boot_cpu: true,
                hardware_id: HardwareCpuId::from_raw(0),
            }; MAX_CPUS],
            mem_count: 1,
            memory_regions: [MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }; 16],
            dev_count,
            devices,
        }
    }

    /// 枚举纯逻辑：ordinal 是 zero-based 匹配序号，越界（含无匹配）→ NoSuchOrdinal；
    /// 一条描述符命中多个 compatible 只计一次；顺序就是设备表顺序（与 claim 无关）。
    #[test]
    fn nth_compatible_counts_once_per_descriptor_and_terminates_with_no_such_ordinal() {
        let mut devices = [DeviceDescriptor::empty(); 26];
        // devices[0] 同时声明两个 compatible —— 匹配任一个都只算一台设备。
        devices[0] = device(
            &[b"virtio,mmio".as_slice(), b"legacy,mmio".as_slice()],
            Some(1),
        );
        devices[1] = device(&[b"ns16550a".as_slice()], Some(10));
        devices[2] = device(&[b"virtio,mmio".as_slice()], Some(2));
        let info = info(devices, 3);

        assert_eq!(
            nth_compatible_in(Some(&info), b"virtio,mmio", 0),
            Ok(DeviceId(0))
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"virtio,mmio", 1),
            Ok(DeviceId(2))
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"virtio,mmio", 2),
            Err(DeviceLookupError::NoSuchOrdinal)
        );
        // 另一个 compatible 命中同一描述符，仍只是 ordinal 0。
        assert_eq!(
            nth_compatible_in(Some(&info), b"legacy,mmio", 0),
            Ok(DeviceId(0))
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"legacy,mmio", 1),
            Err(DeviceLookupError::NoSuchOrdinal)
        );
        assert_eq!(
            nth_compatible_in(Some(&info), b"ns16550a", 0),
            Ok(DeviceId(1))
        );
        // 完全无匹配 → 同样是 NoSuchOrdinal（枚举的唯一终止信号）。
        assert_eq!(
            nth_compatible_in(Some(&info), b"nope,device", 0),
            Err(DeviceLookupError::NoSuchOrdinal)
        );
        // 只看 dev_count 个，尾部空描述符不参与。
        assert_eq!(
            nth_compatible_in(Some(&info), b"virtio,mmio", 2),
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

    /// CompatStr：从字节复制、截断到 32B 容量、`as_str` 往返、空串语义；
    /// Debug 是带引号的字符串（不是 derive 的字段转储）。
    #[test]
    fn compat_str_copies_truncates_and_reports_as_str() {
        // 空串。
        assert_eq!(CompatStr::empty().as_str(), "");
        assert_eq!(alloc::format!("{:?}", CompatStr::empty()), "\"\"");

        // 往返。
        let s = CompatStr::from_bytes(b"virtio,mmio");
        assert_eq!(s.as_str(), "virtio,mmio");
        assert_eq!(alloc::format!("{s:?}"), "\"virtio,mmio\"");

        // 边界：恰好 32B 完整保留；33B 及以上截断到 32B（不 panic）。
        let exact = CompatStr::from_bytes(&[b'x'; 32]);
        assert_eq!(exact.as_str().len(), 32);
        let truncated = CompatStr::from_bytes(&[b'x'; 40]);
        assert_eq!(truncated.as_str().len(), 32, "容量上限是 32B");
        assert!(
            truncated.as_str().bytes().all(|b| b == b'x'),
            "截断保留的是前 32B"
        );
    }

    /// `DeviceDescriptor::matches`：空描述符不匹配任何 compatible；声明的任一
    /// 串命中即为真；只有 `compat_count` 之内的槽位参与匹配。
    #[test]
    fn device_matches_only_declared_compatibles_within_count() {
        // 零个 compatible：无论问什么都是 false。
        assert!(!DeviceDescriptor::empty().matches(b"virtio,mmio"));
        assert!(!DeviceDescriptor::empty().matches(b""));

        // 声明两个 compatible：命中任一为真，无关串为假（前缀也不算命中）。
        let d = device(
            &[b"virtio,mmio".as_slice(), b"legacy,mmio".as_slice()],
            Some(1),
        );
        assert!(d.matches(b"virtio,mmio"), "第一个声明的 compatible");
        assert!(d.matches(b"legacy,mmio"), "第二个声明的 compatible");
        assert!(!d.matches(b"ns16550a"), "未声明的 compatible");
        assert!(!d.matches(b"virtio,mmi"), "前缀不是命中");
        assert!(!d.matches(b""), "空查询不命中非空串");

        // 槽位内容存在，但超出 compat_count 即被忽略。
        let mut clipped = device(
            &[b"virtio,mmio".as_slice(), b"hidden,mmio".as_slice()],
            None,
        );
        assert!(clipped.matches(b"hidden,mmio"), "未截断前参与匹配");
        clipped.compat_count = 1;
        assert!(
            !clipped.matches(b"hidden,mmio"),
            "超出 compat_count 的槽位不得参与匹配"
        );
        assert!(clipped.matches(b"virtio,mmio"), "计数内的槽位仍然命");
    }

    /// DeviceDescriptor Debug：MMIO/PIO 都带空间标签、`write_size` 单位、IRQ
    /// Some/None 与 compatible 列表；不 panic 且含预期子串。
    #[test]
    fn device_descriptor_debug_reports_space_irq_and_compatibles() {
        // Given: 一个带 IRQ 与两个 compatible 的 MMIO 描述符。
        let mut d = device(
            &[b"virtio,mmio".as_slice(), b"legacy,mmio".as_slice()],
            Some(7),
        );
        d.space = IoSpace::Mmio {
            base: 0x1000,
            size: 0x1000,
        };

        // When: 格式化。
        let text = alloc::format!("{d:?}");

        // Then: 空间/大小/IRQ/compatible 都可读。
        assert!(text.contains("DeviceDescriptor"), "{text}");
        assert!(text.contains("mmio: 0x1000"), "{text}");
        assert!(text.contains("size: 4 KiB"), "{text}");
        assert!(text.contains("irq: 7"), "{text}");
        assert!(text.contains("virtio,mmio"), "{text}");
        assert!(text.contains("legacy,mmio"), "{text}");

        // 无 IRQ：显式 `None`（不是省略），零 compatible 打印空列表。
        let mut no_irq = DeviceDescriptor::empty();
        no_irq.space = IoSpace::Mmio { base: 0, size: 0 };
        let text = alloc::format!("{no_irq:?}");
        assert!(text.contains("irq: None"), "{text}");
        assert!(text.contains("compatibles: []"), "{text}");

        // PIO 空间（x86 专用）同样被标注。
        let mut pio = DeviceDescriptor::empty();
        pio.space = IoSpace::Pio {
            base: 0x3f8,
            size: 8,
        };
        let text = alloc::format!("{pio:?}");
        assert!(text.contains("pio: 0x3f8"), "{text}");
        assert!(text.contains("size: 8 B"), "{text}");
    }

    /// MachineInfo Debug：按 count 切片 CPU/内存/设备表，不 panic 且含身份、
    /// 计数与已声明设备；`dev_count == 0` 时为空列表。
    #[test]
    fn machine_info_debug_contains_counts_and_sliced_tables() {
        // Given: 1 CPU / 1 memory region / 3 devices。
        let mut devices = [DeviceDescriptor::empty(); 26];
        devices[0] = device(&[b"virtio,mmio".as_slice()], Some(1));
        devices[1] = device(&[b"ns16550a".as_slice()], Some(10));
        devices[2] = device(&[b"riscv,clint0".as_slice()], None);
        let machine_info = info(devices, 3);

        // When: 格式化（切片 cpu/mem/device 表不得越界）。
        let text = alloc::format!("{machine_info:?}");

        // Then: 身份 + 计数 + 声明设备可见，尾部空槽不可见。
        assert!(text.contains("MachineInfo"), "{text}");
        assert!(
            text.contains("boot_hardware_id: HardwareCpuId(0)"),
            "{text}"
        );
        assert!(text.contains("cpu_count: 1"), "{text}");
        assert!(text.contains("mem_count: 1"), "{text}");
        assert!(text.contains("memory_regions"), "{text}");
        assert!(text.contains("256 MiB"), "{text}");
        assert!(text.contains("dev_count: 3"), "{text}");
        assert!(text.contains("virtio,mmio"), "{text}");
        assert!(text.contains("riscv,clint0"), "{text}");

        // dev_count == 0：空切片正常 Debug，仍然不 panic。
        let empty = info([DeviceDescriptor::empty(); 26], 0);
        let text = alloc::format!("{empty:?}");
        assert!(text.contains("dev_count: 0"), "{text}");
        assert!(text.contains("devices: []"), "{text}");
    }
}
