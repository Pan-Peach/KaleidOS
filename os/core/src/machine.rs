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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CpuId(pub usize);

impl core::fmt::Display for CpuId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "CPU{}", self.0)
    }
}

impl CpuId {
    pub const fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CpuInfo {
    pub boot_cpu: bool,
    pub hart_id: CpuId,
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

#[derive(Clone, Copy)]
pub struct DeviceDescriptor {
    pub mmio_base: usize,
    pub mmio_size: usize,
    pub irq: Option<u32>,
    pub compatibles: [CompatStr; 4],
    pub compat_count: u8,
}

impl DeviceDescriptor {
    pub const fn empty() -> Self {
        Self {
            mmio_base: 0,
            mmio_size: 0,
            irq: None,
            compatibles: [CompatStr::empty(); 4],
            compat_count: 0,
        }
    }
}

impl core::fmt::Debug for DeviceDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "DeviceDescriptor {{ mmio_base: {:#x}, mmio_size: ",
            self.mmio_base
        )?;
        write_size(f, self.mmio_size)?;
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
    pub boot_hart: usize,
    pub cpu_count: usize,
    pub cpu_info: [CpuInfo; 8],
    pub mem_count: usize,
    pub memory_regions: [MemoryRegion; 16],
    pub dev_count: usize,
    pub devices: [DeviceDescriptor; 26],
}

impl core::fmt::Debug for MachineInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MachineInfo")
            .field("boot_hart", &self.boot_hart)
            .field("cpu_count", &self.cpu_count)
            .field("cpu_info", &&self.cpu_info[..self.cpu_count])
            .field("mem_count", &self.mem_count)
            .field("memory_regions", &&self.memory_regions[..self.mem_count])
            .field("dev_count", &self.dev_count)
            .field("devices", &&self.devices[..self.dev_count])
            .finish()
    }
}
