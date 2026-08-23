//! 归一化机器信息（MachineInfo）：bootstrap 发现 → core::init 消费。
//! 单镜像内函数调用交接，用 Rust 类型即可（无需跨 binary POD/协议）。
//! 字段语义：Core Resource Truth 的"提案"，由 core::init 校验后提交。

#[derive(Debug, Clone, Copy)]
pub struct CpuInfo {
    pub boot_cpu: bool,
    pub hart_id: usize,
}

#[derive(Clone, Copy)]
pub struct MemoryRegion {
    pub base: usize,
    pub size: usize,
}

fn write_size(f: &mut core::fmt::Formatter<'_>, bytes: usize) -> core::fmt::Result {
    let bytes = bytes as u64;
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if bytes >= GIB && bytes % GIB == 0 {
        write!(f, "{} GiB", bytes / GIB)
    } else if bytes >= MIB && bytes % MIB == 0 {
        write!(f, "{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes % KIB == 0 {
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

#[derive(Clone, Copy)]
pub struct DeviceDescriptor<'a> {
    pub mmio_base: usize,
    pub mmio_size: usize,
    pub irq: Option<u32>,
    pub compatible: &'a str,
}

impl core::fmt::Debug for DeviceDescriptor<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "DeviceDescriptor {{ mmio_base: {:#x}, mmio_size: ", self.mmio_base)?;
        write_size(f, self.mmio_size)?;
        match self.irq {
            Some(irq) => write!(f, ", irq: {}, compatible: {:?} }}", irq, self.compatible),
            None => write!(f, ", irq: None, compatible: {:?} }}", self.compatible),
        }
    }
}

#[derive(Debug)]
pub struct MachineInfo<'a> {
    pub boot_hart: usize,
    pub cpu_info: &'a [CpuInfo],
    pub memory_regions: &'a [MemoryRegion],
    pub devices: &'a [DeviceDescriptor<'a>],
}
