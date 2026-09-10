//! 长期内核地址空间（**骨架**：契约与调用点就位，语义实现由人类完成）。
//!
//! `super::bootstrap::init` 建立的临时 root 只活到 `kernel::init()` 完成、
//! buddy allocator 可用之前；此后由本模块用 `Sv39AddressSpace`（buddy 动态
//! 页表）建立长期 root，并在 Core 就绪后替换 bootstrap root。
//!
//! 与 bootstrap 的区别只是**实现时机和 backing**，输入是同一个 `KernelLayout`：
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
//! TODO(实现)：
//! 1. `build`：`Sv39AddressSpace::new(kernel::memory::vm_page_alloc, 0)`；
//! 2. map identity RAM（bootstrap 曾用 1 GiB 大叶的粗映射，这里 4 KiB 粒度）；
//! 3. 按 `layout.sections()` 映射内核镜像（.text=RX / .rodata+.initpkg=R /
//!    .data+.bss=RW）——权限与 bootstrap 保证一致（同一 layout 来源）；
//! 4. `verify`（对照 layout 逐段 translate 校验）+ `activate`（satp 切换，
//!    ASID 0，替换 bootstrap root）。
//!
//! Sv39AddressSpace 的机制（map/unmap/translate/权限/失败回滚）已在 arch
//! crate 的 host 测试覆盖；本模块只做编排。boot crate 是 riscv-only 二进制
//! （`test = false`），骨架语义由本 TODO 与 arch 测试共同承接。
//!
//! `#![allow(dead_code)]`：骨架未接线（main64 尚未调用 `RuntimeVm`），实现
//! 语义后随调用点一起移除。

#![allow(dead_code)]

use arch::riscv::mmu::address_space::Sv39AddressSpace;

use super::layout::KernelLayout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeVmError {
    PageTableAllocFailed,
    MapFailed,
    VerifyFailed,
    ActivateFailed,
}

/// 长期内核地址空间（buddy 动态根）。
pub struct RuntimeVm {
    /// Sv39 动态根（buddy 支撑）。字段语义实现后使用；
    /// 骨架阶段不构造，`#[allow(dead_code)]` 待 `build` 落地后移除。
    space: Sv39AddressSpace,
}

impl RuntimeVm {
    /// 建立长期 root（见模块文档 TODO）：
    /// `Sv39AddressSpace::new(kernel::memory::vm_page_alloc, 0)` →
    /// map identity RAM → map `layout.sections()`。
    pub fn build(
        _layout: &KernelLayout,
        _memory: &[kernel::machine::MemoryRegion],
    ) -> Result<Self, RuntimeVmError> {
        todo!("RuntimeVm::build —— Sv39AddressSpace::new(vm_page_alloc, 0) → identity RAM → KernelLayout")
    }

    /// 对照 layout 逐段 translate 校验映射已就位。
    pub fn verify(&self) -> Result<(), RuntimeVmError> {
        todo!("RuntimeVm::verify —— 逐段 translate 校验")
    }

    /// 激活为当前 satp（替换 bootstrap 临时 root，ASID 0）。
    pub fn activate(&self) -> Result<(), RuntimeVmError> {
        todo!("RuntimeVm::activate —— backend activate + sfence")
    }
}
