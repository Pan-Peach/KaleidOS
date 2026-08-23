pub struct CpuInfo {
    pub boot_cpu: bool,
    pub hart_id: usize,
}

pub struct MemoryRegion {
    pub base: usize,
    pub size: usize,
}

pub struct DeviceDescriptor<'a> {
    pub mmio_base: usize,
    pub mmio_size: usize,
    pub irq: Option<usize>,
    pub compatible: &'a str,
}

pub struct MachineInfo<'a> {
    pub cpu_info: &'a [CpuInfo],
    pub memory_regions: &'a [MemoryRegion],
    pub devices: &'a [DeviceDescriptor<'a>],
}
