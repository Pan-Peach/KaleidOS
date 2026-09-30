//! 长期内核地址空间（buddy 动态根）——boot root → runtime root 的交接点。
//!
//! `super::bootstrap::init` 建立的临时 root 只活到 `kernel::init()` 完成、
//! buddy allocator 可用之前；此后本模块用 `Sv39AddressSpace`（buddy 动态页表）
//! 建立长期 root，`init()` 里完成 build → verify → activate 并在全局安装
//! （`main64.rs` 在 `kernel::init()` 后调用一次）。与 bootstrap 的区别只是
//! **实现时机和 backing**，输入是同一个 `KernelLayout`：
//!
//! ```text
//!          KernelLayout（layout.rs，linker symbols 的唯一解释者）
//!                 │
//!         ┌───────┴────────┐
//!         ↓                ↓
//!   bootstrap.rs       runtime.rs
//!   early/static      dynamic/buddy
//!         │                │
//!         └───────┬────────┘
//!                 ↓
//!         Sv39 机制（arch/riscv/mmu）
//! ```
//!
//! # 映射四来源（PA 从哪来）
//!
//! ```text
//! bootstrap 影子 → [KERNEL_VMA, text.va_start) 高半区（含 .bss.stack 高栈，RW）
//! layout       → 高半区正式段（.text RX / .rodata+.initpkg R / .data+.bss RW）
//!               → PA = kernel_pa + (va - KERNEL_VMA)
//! info RAM     → identity 映射（VA == PA，RWX：组件池在 buddy identity 页执行）
//! info firmware→ 保留的 FDT 落在 identity RAM 之外时的显式只读 identity 映射
//! info 设备    → MMIO 区间（VA == PA，RW-NX，页对齐向外取整）
//! ```
//!
//! identity RAM 目前带 `EXECUTE`：loader 在组件池（buddy identity 页）里跑代码；
//! RAM 的 usable 边界做**内向取整**（宁少不越界），设备 MMIO 向外取整。
//!
//! `Sv39AddressSpace` 的机制（map/unmap/translate/权限/失败回滚）已在 arch crate
//! 的 host 测试覆盖；本模块只做编排。boot crate 是 riscv-only 二进制
//! （`test = false`），编排逻辑由 QEMU 端到端验证承接。

use arch::riscv::mmu::address_space::Sv39AddressSpace;
use arch::riscv::mmu::sv39::{PteFlags, VM_PAGE_SIZE};
use arch::vm::{AddressSpaceBackend, MappingPermission, PhysicalRange, VirtualRange};
use kernel::machine::{FirmwareInfo, IoSpace, MachineInfo};
use kernel::memory::address_space::Mapping;
use kernel::memory::kernel_mappings::{KernelMappingPlan, MappingClass};
use spin::{Mutex, Once};

use super::layout::{KernelLayout, KERNEL_VMA};

/// 全局长期内核地址空间（`init` 安装；驱动/可执行区/直接映射等消费方
/// 从这里拿 `&mut RuntimeVm` 追加映射）。
pub static RUNTIME_VM: Once<Mutex<Option<RuntimeVm>>> = Once::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeVmError {
    /// 页表页分配失败（buddy 耗尽）。
    PageTableAllocFailed,
    /// map 失败（未对齐 / 已映射 / backend 错误）。
    MapFailed,
    /// verify 未通过（段首翻译结果与期望 PA 不符）。
    VerifyFailed,
    /// activate（satp 切换）失败。
    ActivateFailed,
    /// 布局非法（段越界 / 算术溢出）。
    InvalidLayout,
    /// `init` 重复调用（长期 root 只安装一次）。
    AlreadyInstalled,
}

/// 长期内核地址空间（buddy 动态根）。
pub struct RuntimeVm {
    space: Sv39AddressSpace,
    /// 每次映射操作的语义分类（共享 Core / identity RAM / 只留内核 root），
    /// `init` 把它交给 Core（`kernel_mappings::install`），供后续 Isolated AS
    /// 共享同一份 same VA → same PA 的 Core 映射。
    plan: KernelMappingPlan,
}

const fn align_up_page(addr: usize) -> usize {
    (addr + VM_PAGE_SIZE - 1) & !(VM_PAGE_SIZE - 1)
}

const fn align_down_page(addr: usize) -> usize {
    addr & !(VM_PAGE_SIZE - 1)
}

/// layout 的 `PteFlags`（arch 编码）→ 逻辑 `MappingPermission`（backend API）。
/// 两个位集合语义一致（R/W/X/U），只做位搬运。
fn section_perm(flags: PteFlags) -> MappingPermission {
    let mut perm = MappingPermission::empty();
    if flags.contains(PteFlags::R) {
        perm.insert(MappingPermission::READ);
    }
    if flags.contains(PteFlags::W) {
        perm.insert(MappingPermission::WRITE);
    }
    if flags.contains(PteFlags::X) {
        perm.insert(MappingPermission::EXECUTE);
    }
    if flags.contains(PteFlags::U) {
        perm.insert(MappingPermission::USER);
    }
    perm
}

/// 中断控制器（PLIC）：Core 的 trap 路径在**每个** AS 里都要 claim/complete
/// 外部中断，因此它的 MMIO 窗口属于共享 Core 映射；其余设备窗口只留内核 root
/// （Isolated 组件不因共享而获得设备访问权）。
fn is_interrupt_controller(device: &kernel::machine::DeviceDescriptor) -> bool {
    device.compatibles[..device.compat_count as usize]
        .iter()
        .any(|compatible| matches!(compatible.as_str(), "riscv,plic0" | "sifive,plic-1.0.0"))
}

/// 段的高半区 VMA → 段在物理镜像内的 PA。
fn section_pa(va_start: usize, kernel_pa: usize) -> Result<usize, RuntimeVmError> {
    let offset = va_start
        .checked_sub(KERNEL_VMA)
        .ok_or(RuntimeVmError::InvalidLayout)?;
    kernel_pa
        .checked_add(offset)
        .ok_or(RuntimeVmError::InvalidLayout)
}

impl RuntimeVm {
    /// 建立长期 root。映射来源覆盖全部 PA 来源（见模块文档）：
    /// 0. 低引导区的高半区影子（`[KERNEL_VMA, text.va_start)`，RW-NX——含
    ///    `.bss.stack` 高栈与 `.bss.early_root`，bootstrap_high 正跑在上面）；
    /// 1. identity RAM（`info.memory_regions`，VA == PA，RWX——阶段一组件池）；
    ///    1b. 保留固件源（`info.firmware` 的 FDT 不在 identity RAM 内时的只读兜底）；
    /// 2. 内核镜像正式段（`layout.sections()`，PA 由 `kernel_pa` 推导，段权限）；
    /// 3. 设备 MMIO（`info.devices`，VA == PA，RW-NX）。
    pub fn build(
        layout: &KernelLayout,
        kernel_pa: usize,
        info: &MachineInfo,
    ) -> Result<Self, RuntimeVmError> {
        let mut space = Sv39AddressSpace::new(kernel::memory::vm_page_alloc, 0)
            .map_err(|_| RuntimeVmError::PageTableAllocFailed)?;
        let mut plan = KernelMappingPlan::empty();

        macro_rules! record {
            ($class:expr, $mapping:expr) => {
                plan.add($class, $mapping, VM_PAGE_SIZE)
                    .map_err(|_| RuntimeVmError::MapFailed)?;
            };
        }

        // 0) 低引导区高半区影子 [KERNEL_VMA, text.va_start)：bootstrap 用
        //    "Pass 1 整镜像 RW" 覆盖它；runtime 只映正式段之前的影子
        //    （正式段彼此页对齐贴齐、无 gap）。`.bss.stack` 高栈在这里，
        //    activate 后 CPU 还在其上运行——漏了必 fault。
        let shadow_size = layout.text.va_start - KERNEL_VMA;
        if shadow_size > 0 {
            let shadow_perm = MappingPermission::READ | MappingPermission::WRITE;
            let va = VirtualRange {
                base: KERNEL_VMA,
                size: shadow_size,
            };
            let pa = PhysicalRange {
                base: kernel_pa,
                size: shadow_size,
            };
            space
                .map(va, pa, shadow_perm)
                .map_err(|_| RuntimeVmError::MapFailed)?;
            // 只属于内核 root：bootstrap 影子不共享进实例 AS。
            record!(
                MappingClass::CoreRootOnly,
                Mapping {
                    virtual_range: va,
                    physical_range: pa,
                    permission: shadow_perm,
                }
            );
        }

        // 1) identity RAM：VA == PA。bootstrap 曾用 1 GiB 大叶的粗映射，这里
        //    4 KiB 粒度。阶段一带 EXECUTE：组件池在 buddy identity 页里执行
        //    （loader 从那里跑代码）。
        //    对齐：只向内取整（start 向上、end 向下）——绝不把映射扩大到
        //    机器报告的 RAM 边界之外（边缘页可能混着 reserved/非 RAM）。
        let ram_perm =
            MappingPermission::READ | MappingPermission::WRITE | MappingPermission::EXECUTE;
        for r in info.memory_regions.iter() {
            if r.size == 0 {
                continue;
            }
            let end = r
                .base
                .checked_add(r.size)
                .ok_or(RuntimeVmError::InvalidLayout)?;
            let start = align_up_page(r.base);
            let end = align_down_page(end);
            if end <= start {
                continue; // 取整后为空（非对齐 RAM 的边缘碎片）
            }
            let range = VirtualRange {
                base: start,
                size: end - start,
            };
            let pa = PhysicalRange {
                base: start,
                size: end - start,
            };
            space
                .map(range, pa, ram_perm)
                .map_err(|_| RuntimeVmError::MapFailed)?;
            // identity RAM：共享，但私有 backing 的别名必须可被整段摘除
            // （`kernel_mappings::publish_private_backing`）。
            record!(
                MappingClass::SharedIdentity,
                Mapping {
                    virtual_range: range,
                    physical_range: pa,
                    permission: ram_perm,
                }
            );
        }

        // 1b) 保留的固件源（`info.firmware`）：identity RAM 映射只覆盖
        //     `memory_regions`（且向内取整到页）。保留的 FDT 若落在这些映射
        //     之外，**物理驻留 ≠ 可访问**——显式建立 Core 可访问的只读
        //     identity 映射。只进内核 root：不给实例任何 PA 查询 / 共享别名。
        if let FirmwareInfo::Fdt { phys, size } = info.firmware {
            if size == 0 {
                return Err(RuntimeVmError::InvalidLayout);
            }
            let end = phys
                .checked_add(size)
                .ok_or(RuntimeVmError::InvalidLayout)?;
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
                let range = VirtualRange {
                    base: start_page,
                    size: end_page - start_page,
                };
                let pa = PhysicalRange {
                    base: start_page,
                    size: end_page - start_page,
                };
                let permission = MappingPermission::READ;
                space
                    .map(range, pa, permission)
                    .map_err(|_| RuntimeVmError::MapFailed)?;
                record!(
                    MappingClass::CoreRootOnly,
                    Mapping {
                        virtual_range: range,
                        physical_range: pa,
                        permission,
                    }
                );
            }
        }

        // 2) 内核镜像：段 VA 高半区，PA = kernel_pa + (va_start - KERNEL_VMA)。
        //    段 end 未必页对齐（如 .text 止于 ..96d6），必须 round_up 到 4K；
        //    map_range 要求 size 页对齐（sv39.rs 的 is_page_aligned 校验）。
        for section in layout.sections() {
            if section.va_start == section.va_end {
                continue; // 空段（如未内嵌 .initpkg 时）
            }
            if section.va_start & (VM_PAGE_SIZE - 1) != 0 {
                return Err(RuntimeVmError::InvalidLayout);
            }
            let pa = section_pa(section.va_start, kernel_pa)?;
            let size = align_up_page(
                section
                    .va_end
                    .checked_sub(section.va_start)
                    .ok_or(RuntimeVmError::InvalidLayout)?,
            );
            let va = VirtualRange {
                base: section.va_start,
                size,
            };
            let pa = PhysicalRange { base: pa, size };
            let permission = section_perm(section.flags);
            space
                .map(va, pa, permission)
                .map_err(|_| RuntimeVmError::MapFailed)?;
            // 固定 Core 镜像段：每个 Isolated AS 共享（同 VA → 同 PA）。
            record!(
                MappingClass::SharedCore,
                Mapping {
                    virtual_range: va,
                    physical_range: pa,
                    permission,
                }
            );
        }

        // 3) 设备 MMIO：VA == PA，RW-NX（设备寄存器永不执行）。
        //    FDT 的 reg 区间未必页对齐，映射覆盖它的整页窗口（MMU 只能按页，
        //    向外取整可接受；与 RAM 的内向取整不同——MMIO 页不会混着 RAM）。
        let mmio_perm = MappingPermission::READ | MappingPermission::WRITE;
        for d in info.devices.iter() {
            if let IoSpace::Mmio { base, size } = d.space {
                if size == 0 {
                    continue;
                }
                let map_base = base & !(VM_PAGE_SIZE - 1);
                let map_end = base
                    .checked_add(size)
                    .map(align_up_page)
                    .ok_or(RuntimeVmError::InvalidLayout)?;
                let map_size = map_end - map_base;
                let range = VirtualRange {
                    base: map_base,
                    size: map_size,
                };
                let pa = PhysicalRange {
                    base: map_base,
                    size: map_size,
                };
                space
                    .map(range, pa, mmio_perm)
                    .map_err(|_| RuntimeVmError::MapFailed)?;
                // 中断控制器窗口共享（trap 路径在每个 AS 都要 claim / complete）；
                // 其余设备窗口只留内核 root。
                let class = if is_interrupt_controller(d) {
                    MappingClass::SharedCore
                } else {
                    MappingClass::CoreRootOnly
                };
                record!(
                    class,
                    Mapping {
                        virtual_range: range,
                        physical_range: pa,
                        permission: mmio_perm,
                    }
                );
            }
        }

        Ok(Self { space, plan })
    }

    /// 对照 layout 逐段校验：段首地址必须能翻译回期望 PA。
    ///
    /// 注意：`translate` 只证明"映上了且 PA 对"，验证不了 PTE 权限位
    /// （backend 不暴露权限位）。
    pub fn verify(&self, layout: &KernelLayout, kernel_pa: usize) -> Result<(), RuntimeVmError> {
        // 低引导影子（高栈）首地址必须可翻译回 kernel_pa。
        if layout.text.va_start > KERNEL_VMA {
            match self.space.translate(KERNEL_VMA) {
                Some(pa) if pa == kernel_pa => {}
                _ => return Err(RuntimeVmError::VerifyFailed),
            }
        }
        for section in layout.sections() {
            if section.va_start == section.va_end {
                continue;
            }
            let expected_pa = section_pa(section.va_start, kernel_pa)?;
            match self.space.translate(section.va_start) {
                Some(pa) if pa == expected_pa => {}
                _ => return Err(RuntimeVmError::VerifyFailed),
            }
        }
        Ok(())
    }

    /// 激活为当前 satp（替换 bootstrap 临时 root，ASID 0）。
    pub fn activate(&self) -> Result<(), RuntimeVmError> {
        self.space
            .activate()
            .map_err(|_| RuntimeVmError::ActivateFailed)
    }

    /// 取走映射计划（交给 Core 的共享映射真相；调用后本 VM 不再持有它）。
    pub fn take_plan(&mut self) -> KernelMappingPlan {
        core::mem::take(&mut self.plan)
    }
}

/// 建立、验证、激活并安装全局长期 root（`main64.rs` 在 `kernel::init()` 后
/// 调用一次；重复调用返回 `AlreadyInstalled`）。
///
/// 安装顺序：先检查是否已安装 → build → verify → activate → 存入全局。
/// build 失败时全局槽保持空，调用方（或修复后重试）可以再试。
pub fn init(
    layout: &KernelLayout,
    kernel_pa: usize,
    info: &MachineInfo,
) -> Result<(), RuntimeVmError> {
    let slot = RUNTIME_VM.call_once(|| Mutex::new(None));
    if slot.lock().is_some() {
        return Err(RuntimeVmError::AlreadyInstalled);
    }

    let mut vm = RuntimeVm::build(layout, kernel_pa, info)?;
    vm.verify(layout, kernel_pa)?;
    vm.activate()?;

    // 把映射计划交给 Core：Isolated AS 从这里取共享 Core 映射
    // （same VA → same PA），私有 backing 的别名排除也以它为真相。
    let plan = vm.take_plan();
    kernel::memory::kernel_mappings::install(plan);

    *slot.lock() = Some(vm);
    Ok(())
}
