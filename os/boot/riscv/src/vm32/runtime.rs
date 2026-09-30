//! RV32 长期 root（buddy 动态 Sv32 页表）：替换 `entry32.S` 的全量 executable
//! bootstrap root。
//!
//! 映射四来源（与 RV64 `vm/runtime.rs` 同构，identity 取代高半区）：
//!
//! ```text
//! linker32.ld 段 + layout  →  镜像段（VA==PA，段权限）
//! info RAM                 →  identity RAM（VA==PA，RWX；**挖掉镜像段区间**）
//! info firmware            →  保留 FDT 落在 identity RAM 之外时的显式只读 identity 映射
//! info 设备                →  MMIO 窗口（VA==PA，RW-NX，页对齐向外取整）
//! ```
//!
//! 每次映射操作都带语义分类进 [`KernelMappingPlan`]，`init` 在激活后把计划交给
//! Core：Isolated AS 只从这份计划取共享 Core 映射，绝不从 bootstrap root 推导。
//!
//! `VM_PAGE_SIZE` 用 Sv32 backend 的粒度（4 KiB，与 RV64 一致）。

use arch::vm::{AddressSpaceBackend, MappingPermission, PhysicalRange, VirtualRange};
use arch::AddressSpaceImpl;
use kernel::machine::{FirmwareInfo, IoSpace, MachineInfo};
use kernel::memory::address_space::Mapping;
use kernel::memory::kernel_mappings::{KernelMappingPlan, MappingClass};
use spin::Mutex;

use super::layout::{KernelLayout32, KernelSection32};

const PAGE: usize = 4096;

/// 已安装的长期 RV32 root（`init` 后可供诊断；重复 `init` 拒绝）。
pub static RUNTIME_VM32: Mutex<Option<RuntimeVm32>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeVm32Error {
    /// 页表页分配失败（buddy 耗尽）。
    PageTableAllocFailed,
    /// map 失败（未对齐 / 已映射 / backend 错误）。
    MapFailed,
    /// verify 未通过（段首翻译结果与期望 PA 不符）。
    VerifyFailed,
    /// 布局非法（段越界 / 算术溢出 / 镜像不在其链接地址上）。
    InvalidLayout,
    /// activate（satp 切换）失败。
    ActivateFailed,
    /// `init` 重复调用。
    AlreadyInstalled,
}

/// 长期 RV32 内核地址空间。
pub struct RuntimeVm32 {
    space: AddressSpaceImpl,
    plan: KernelMappingPlan,
}

const fn align_up_page(addr: usize) -> usize {
    (addr + PAGE - 1) & !(PAGE - 1)
}

const fn align_down_page(addr: usize) -> usize {
    addr & !(PAGE - 1)
}

/// 中断控制器（PLIC）窗口：Core trap 路径在每个 AS 里都要 claim / complete，
/// 因此共享；其余设备窗口只留内核 root。
fn is_interrupt_controller(device: &kernel::machine::DeviceDescriptor) -> bool {
    device.compatibles[..device.compat_count as usize]
        .iter()
        .any(|compatible| matches!(compatible.as_str(), "riscv,plic0" | "sifive,plic-1.0.0"))
}

/// `[start, end)` 里减去镜像区间后的最多两段：`(pieces, count)`，无分配。
fn subtract_image(
    start: usize,
    end: usize,
    image_start: usize,
    image_end: usize,
) -> ([(usize, usize); 2], usize) {
    if end <= image_start || start >= image_end {
        return ([(start, end), (0, 0)], 1);
    }
    let mut pieces = [(0usize, 0usize); 2];
    let mut count = 0;
    if start < image_start {
        pieces[count] = (start, image_start);
        count += 1;
    }
    if image_end < end {
        pieces[count] = (image_end, end);
        count += 1;
    }
    (pieces, count)
}

impl RuntimeVm32 {
    /// 建立长期 root（**未激活**）。`kernel_pa` 必须是镜像链接地址（RV32 的
    /// identity 视图要求加载地址 == 链接地址，QEMU/OpenSBI 满足）。
    pub fn build(
        layout: &KernelLayout32,
        kernel_pa: usize,
        info: &MachineInfo,
    ) -> Result<Self, RuntimeVm32Error> {
        let image_start = layout.text.va_start;
        if kernel_pa != image_start || kernel_pa & (PAGE - 1) != 0 {
            return Err(RuntimeVm32Error::InvalidLayout);
        }
        let image_end = layout.bss.va_end;
        if image_end < image_start {
            return Err(RuntimeVm32Error::InvalidLayout);
        }

        let mut space =
            <AddressSpaceImpl as AddressSpaceBackend>::create(kernel::memory::vm_page_alloc)
                .map_err(|_| RuntimeVm32Error::PageTableAllocFailed)?;
        let mut plan = KernelMappingPlan::empty();

        macro_rules! map_and_record {
            ($class:expr, $va:expr, $pa:expr, $perm:expr) => {{
                let va = VirtualRange {
                    base: $va,
                    size: $pa.size,
                };
                space
                    .map(va, $pa, $perm)
                    .map_err(|_| RuntimeVm32Error::MapFailed)?;
                plan.add(
                    $class,
                    Mapping {
                        virtual_range: va,
                        physical_range: $pa,
                        permission: $perm,
                    },
                    PAGE,
                )
                .map_err(|_| RuntimeVm32Error::MapFailed)?;
            }};
        }

        // 1) identity RAM：VA == PA，RWX（KernelNative 组件池在 buddy 页上执行）。
        //    镜像区间由下面的段映射覆盖（R+X / R / RW），这里挖掉。
        let ram_perm =
            MappingPermission::READ | MappingPermission::WRITE | MappingPermission::EXECUTE;
        let image_va_end = align_up_page(image_end);
        for r in info.memory_regions.iter() {
            if r.size == 0 {
                continue;
            }
            let Some(region_end) = r.base.checked_add(r.size) else {
                return Err(RuntimeVm32Error::InvalidLayout);
            };
            let start = align_up_page(r.base);
            let end = align_down_page(region_end);
            if end <= start {
                continue;
            }
            let (pieces, count) = subtract_image(start, end, image_start, image_va_end);
            for (piece_start, piece_end) in pieces[..count].iter().copied() {
                if piece_end <= piece_start {
                    continue;
                }
                map_and_record!(
                    MappingClass::SharedIdentity,
                    piece_start,
                    PhysicalRange {
                        base: piece_start,
                        size: piece_end - piece_start,
                    },
                    ram_perm
                );
            }
        }

        // 1b) 保留的固件源（`info.firmware`）：identity RAM 映射只覆盖
        //     `memory_regions`（且挖掉镜像 / 向内取整）。保留的 FDT 若落在这些
        //     映射之外，**物理驻留 ≠ 可访问**——显式建立 Core 可访问的只读
        //     identity 映射。只进内核 root：不给实例任何 PA 查询 / 共享别名。
        if let FirmwareInfo::Fdt { phys, size } = info.firmware {
            if size == 0 {
                return Err(RuntimeVm32Error::InvalidLayout);
            }
            let end = phys
                .checked_add(size)
                .ok_or(RuntimeVm32Error::InvalidLayout)?;
            let start_page = align_down_page(phys);
            let end_page = align_up_page(end);
            let covered = info.memory_regions.iter().any(|region| {
                let Some(region_end) = region.base.checked_add(region.size) else {
                    return false;
                };
                // 与上面的 identity 映射同一取整：`[align_up(base), align_down(end))`。
                align_up_page(region.base) <= start_page && end_page <= align_down_page(region_end)
            });
            if !covered {
                map_and_record!(
                    MappingClass::CoreRootOnly,
                    start_page,
                    PhysicalRange {
                        base: start_page,
                        size: end_page - start_page,
                    },
                    MappingPermission::READ
                );
            }
        }

        // 2) 镜像段：identity VA == PA，段权限（RX / R / RW）。
        for section in layout.sections() {
            if section.va_start == section.va_end {
                continue;
            }
            map_section(&mut space, &mut plan, section)?;
        }

        // 3) 设备 MMIO：VA == PA，RW-NX，页对齐向外取整。
        let mmio_perm = MappingPermission::READ | MappingPermission::WRITE;
        for d in info.devices.iter() {
            if let IoSpace::Mmio { base, size } = d.space {
                if size == 0 {
                    continue;
                }
                let map_base = base & !(PAGE - 1);
                let map_end = base
                    .checked_add(size)
                    .map(align_up_page)
                    .ok_or(RuntimeVm32Error::InvalidLayout)?;
                let map_size = map_end - map_base;
                let class = if is_interrupt_controller(d) {
                    MappingClass::SharedCore
                } else {
                    MappingClass::CoreRootOnly
                };
                map_and_record!(
                    class,
                    map_base,
                    PhysicalRange {
                        base: map_base,
                        size: map_size,
                    },
                    mmio_perm
                );
            }
        }

        Ok(Self { space, plan })
    }

    /// 对照布局校验：段首地址必须能翻译回同一地址（identity）。
    pub fn verify(&self, layout: &KernelLayout32) -> Result<(), RuntimeVm32Error> {
        for section in layout.sections() {
            if section.va_start == section.va_end {
                continue;
            }
            match self.space.translate(section.va_start) {
                Some(pa) if pa == section.va_start => {}
                _ => return Err(RuntimeVm32Error::VerifyFailed),
            }
        }
        Ok(())
    }

    /// 激活为当前 satp（替换 bootstrap 临时 root，ASID 0）。
    pub fn activate(&self) -> Result<(), RuntimeVm32Error> {
        self.space
            .activate()
            .map_err(|_| RuntimeVm32Error::ActivateFailed)
    }

    /// 取走映射计划（交给 Core；调用后本 VM 不再持有它）。
    pub fn take_plan(&mut self) -> KernelMappingPlan {
        core::mem::take(&mut self.plan)
    }
}

fn map_section(
    space: &mut AddressSpaceImpl,
    plan: &mut KernelMappingPlan,
    section: KernelSection32,
) -> Result<(), RuntimeVm32Error> {
    let Some(size) = section
        .va_end
        .checked_sub(section.va_start)
        .map(align_up_page)
    else {
        return Err(RuntimeVm32Error::InvalidLayout);
    };
    let va = VirtualRange {
        base: section.va_start,
        size,
    };
    let pa = PhysicalRange {
        base: section.va_start,
        size,
    };
    space
        .map(va, pa, section.permission)
        .map_err(|_| RuntimeVm32Error::MapFailed)?;
    plan.add(
        MappingClass::SharedCore,
        Mapping {
            virtual_range: va,
            physical_range: pa,
            permission: section.permission,
        },
        PAGE,
    )
    .map_err(|_| RuntimeVm32Error::MapFailed)?;
    Ok(())
}

/// 建立、验证、激活并安装长期 RV32 root（`main32.rs` 在 `kernel::init()` 后
/// 调用一次；重复调用返回 `AlreadyInstalled`）。
pub fn init(
    layout: &KernelLayout32,
    kernel_pa: usize,
    info: &MachineInfo,
) -> Result<(), RuntimeVm32Error> {
    let mut installed = RUNTIME_VM32.lock();
    if installed.is_some() {
        return Err(RuntimeVm32Error::AlreadyInstalled);
    }
    let mut vm = RuntimeVm32::build(layout, kernel_pa, info)?;
    vm.verify(layout)?;
    vm.activate()?;
    let plan = vm.take_plan();
    kernel::memory::kernel_mappings::install(plan);
    *installed = Some(vm);
    Ok(())
}
