//! 归一化机器信息（MachineInfo）：bootstrap 发现 → core::init 消费。
//! 单镜像内函数调用交接，用 Rust 类型即可（无需跨 binary POD/协议）。
//! 字段语义：Core Resource Truth 的"提案"，由 core::init 校验后提交。

#[derive(Debug, Clone, Copy)]
pub struct CpuInfo {
    pub boot_cpu: bool,
    pub hart_id: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    pub base: usize,
    pub size: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceDescriptor<'a> {
    pub mmio_base: usize,
    pub mmio_size: usize,
    pub irq: Option<u32>,
    pub compatible: &'a str,
}

#[derive(Debug)]
pub struct MachineInfo<'a> {
    pub boot_hart: usize,
    pub cpu_info: &'a [CpuInfo],
    pub memory_regions: &'a [MemoryRegion],
    pub devices: &'a [DeviceDescriptor<'a>],
}
