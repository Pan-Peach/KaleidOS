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
use kernel::machine::{IoSpace, MachineInfo};
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
    /// 建立长期 root。四个映射来源覆盖全部 PA 来源（见模块文档）：
    /// 0. 低引导区的高半区影子（`[KERNEL_VMA, text.va_start)`，RW-NX——含
    ///    `.bss.stack` 高栈与 `.bss.early_root`，bootstrap_high 正跑在上面）；
    /// 1. identity RAM（`info.memory_regions`，VA == PA，RWX——阶段一组件池）；
    /// 2. 内核镜像正式段（`layout.sections()`，PA 由 `kernel_pa` 推导，段权限）；
    /// 3. 设备 MMIO（`info.devices`，VA == PA，RW-NX）。
    pub fn build(
        layout: &KernelLayout,
        kernel_pa: usize,
        info: &MachineInfo,
    ) -> Result<Self, RuntimeVmError> {
        let mut space = Sv39AddressSpace::new(kernel::memory::vm_page_alloc, 0)
            .map_err(|_| RuntimeVmError::PageTableAllocFailed)?;

        // 0) 低引导区高半区影子 [KERNEL_VMA, text.va_start)：bootstrap 用
        //    "Pass 1 整镜像 RW" 覆盖它；runtime 只映正式段之前的影子
        //    （正式段彼此页对齐贴齐、无 gap）。`.bss.stack` 高栈在这里，
        //    activate 后 CPU 还在其上运行——漏了必 fault。
        let shadow_size = layout.text.va_start - KERNEL_VMA;
        if shadow_size > 0 {
            let shadow_perm = MappingPermission::READ | MappingPermission::WRITE;
            space
                .map(
                    VirtualRange {
                        base: KERNEL_VMA,
                        size: shadow_size,
                    },
                    PhysicalRange {
                        base: kernel_pa,
                        size: shadow_size,
                    },
                    shadow_perm,
                )
                .map_err(|_| RuntimeVmError::MapFailed)?;
        }

        // 1) identity RAM：VA == PA。bootstrap 曾用 1 GiB 大叶的粗映射，这里
        //    4 KiB 粒度。阶段一带 EXECUTE：组件池在 buddy identity 页里执行
        //    （loader 从那里跑代码）。
        //    对齐：只向内取整（start 向上、end 向下）——绝不把映射扩大到
        //    机器报告的 RAM 边界之外（边缘页可能混着 reserved/非 RAM）。
        let ram_perm =
            MappingPermission::READ | MappingPermission::WRITE | MappingPermission::EXECUTE;
        for r in &info.memory_regions[..info.mem_count] {
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
            space
                .map(
                    VirtualRange {
                        base: start,
                        size: end - start,
                    },
                    PhysicalRange {
                        base: start,
                        size: end - start,
                    },
                    ram_perm,
                )
                .map_err(|_| RuntimeVmError::MapFailed)?;
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
            space
                .map(
                    VirtualRange {
                        base: section.va_start,
                        size,
                    },
                    PhysicalRange { base: pa, size },
                    section_perm(section.flags),
                )
                .map_err(|_| RuntimeVmError::MapFailed)?;
        }

        // 3) 设备 MMIO：VA == PA，RW-NX（设备寄存器永不执行）。
        //    FDT 的 reg 区间未必页对齐，映射覆盖它的整页窗口（MMU 只能按页，
        //    向外取整可接受；与 RAM 的内向取整不同——MMIO 页不会混着 RAM）。
        let mmio_perm = MappingPermission::READ | MappingPermission::WRITE;
        for d in &info.devices[..info.dev_count] {
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
                space
                    .map(
                        VirtualRange {
                            base: map_base,
                            size: map_size,
                        },
                        PhysicalRange {
                            base: map_base,
                            size: map_size,
                        },
                        mmio_perm,
                    )
                    .map_err(|_| RuntimeVmError::MapFailed)?;
            }
        }

        Ok(Self { space })
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

    let vm = RuntimeVm::build(layout, kernel_pa, info)?;
    vm.verify(layout, kernel_pa)?;
    vm.activate()?;

    *slot.lock() = Some(vm);
    Ok(())
}
