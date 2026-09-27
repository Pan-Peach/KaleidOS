//! RISC-V address-translation backends.
//!
//! The family-level RISC-V module owns the ISA and firmware pieces; this
//! module owns the translation mechanism boundary.  The active XLEN selects
//! the Sv39 (RV64) or Sv32 (RV32) backend at compile time.
//!
//! 本层只保留**机制**：页表编码/遍历（sv39/sv32）、`activate`（satp+sfence）、
//! `flush_tlb`。boot 期的映射策略（identity + high-half 双映射、段权限、
//! 临时 root、enter_high_half）已移出 arch，见 boot crate `vm/`——arch 不
//! 知道 `KERNEL_VMA` / `.text` / `.initpkg` / bootstrap hand-off。

#[cfg(all(
    feature = "vm-mmu",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
pub mod address_space;
// sv32/sv39 是纯逻辑 + identity 指针解引用；host 的 cfg(test)（64 位）也编译，
// 便于 host 测试页表编码与 walk（页面 backing 由测试提供）。
#[cfg(all(
    feature = "vm-mmu",
    any(target_arch = "riscv32", all(test, target_pointer_width = "64"))
))]
pub mod sv32;
#[cfg(all(
    feature = "vm-mmu",
    any(target_arch = "riscv64", all(test, target_pointer_width = "64"))
))]
pub mod sv39;

#[cfg(all(feature = "vm-mmu", test, target_pointer_width = "64"))]
pub(crate) mod test_pool;

#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
const SV39_MODE: usize = 8;
#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
const SV32_MODE: usize = 1;

/// 组装 satp 原始值（模式 | ASID | root PPN）。`activate` 与
/// [`SatpActivation::satp`] 共用，保证两条路径的编码一致。
#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
const fn satp_value(root_ppn: usize, asid: u16) -> usize {
    (SV39_MODE << 60) | ((asid as usize) << 44) | root_ppn
}

#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
const fn satp_value(root_ppn: usize, asid: u16) -> usize {
    (SV32_MODE << 31) | ((asid as usize) << 22) | root_ppn
}

/// 一次 satp 切换所需的原始数据（`AddressSpaceBackend::Activation` 的 RISC-V 形态）。
///
/// 只打包 `root_ppn` + `asid`，**不动寄存器**：真正的 `csrw satp` 仍在
/// [`activate`]。Core 的切换汇编消费本值（`satp()` 给出预打包的 satp 字），
/// 因此它必须是 `Copy` 且不携带借用。ASID 当前恒为 0——`sv39` / `sv32` backend
/// 都不声明 ASID 支持，切换靠 `sfence.vma` 全清。
#[cfg(all(
    feature = "vm-mmu",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SatpActivation {
    /// 根页表物理页号（`PA >> 12`，写 satp 的 PPN 字段）。
    pub root_ppn: usize,
    /// 该地址空间的 ASID（当前恒 0）。
    pub asid: u16,
}

#[cfg(all(
    feature = "vm-mmu",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
impl SatpActivation {
    /// 预打包的 satp 值（模式 | ASID | root PPN）——汇编只需一条 `csrw satp`。
    pub const fn satp(self) -> usize {
        satp_value(self.root_ppn, self.asid)
    }
}

#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
pub type AddressSpace = address_space::Sv39AddressSpace;
#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
pub type AddressSpace = address_space::Sv32AddressSpace;

/// Flush this hart's TLB (`sfence.vma`).
///
/// # Safety
///
/// `SFENCE.VMA` is an S-mode (or higher) instruction. The caller must run with
/// sufficient privilege, and must have published any page-table edits this
/// flush is meant to order (the stores happen-before the flush on this hart).
/// The flush only covers this hart's translations; a hart that will use the
/// updated tables must flush for itself.
#[cfg(all(
    feature = "vm-mmu",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
pub unsafe fn flush_tlb() {
    unsafe {
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
    }
}

/// 当前 `satp` 原值（故障归因：确认被打断的执行确实在预期的实例 root 上）。
#[cfg(all(
    feature = "vm-mmu",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
pub fn current_satp() -> usize {
    let satp: usize;
    // SAFETY: CSR read only; no memory / stack effects.
    unsafe {
        core::arch::asm!(
            "csrr {satp}, satp",
            satp = out(reg) satp,
            options(nostack, preserves_flags),
        );
    }
    satp
}

/// 写 satp 并 flush TLB。这是本模块唯一职责：只碰寄存器，不懂地址空间生命周期。
/// `root_ppn` 是根页表物理页号；`asid` 是该地址空间的 ASID。
///
/// # Safety
///
/// `CSRW satp` is an S-mode (or higher) instruction, and the write takes effect
/// immediately for this hart (after the surrounding `sfence.vma`). The caller
/// must run with sufficient privilege and must pass the PPN of a page-aligned,
/// valid root page table that keeps the caller's own current execution
/// reachable (code, stack, and any memory touched after this call) — otherwise
/// the next fetch faults. `asid` must be the ASID the root was built for and
/// must match the surrounding address-space switching discipline (currently
/// always 0).
#[cfg(all(feature = "vm-mmu", target_arch = "riscv64"))]
pub unsafe fn activate(root_ppn: usize, asid: u16) {
    let satp = satp_value(root_ppn, asid);

    unsafe {
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
        core::arch::asm!(
            "csrw satp, {satp}",
            satp = in(reg) satp,
            options(nostack, preserves_flags),
        );
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
    }
}

/// Write an Sv32 `satp` value and flush stale translations.
///
/// # Safety
///
/// Same obligations as the RV64 variant: S-mode (or higher) only; `root_ppn`
/// must reference a page-aligned, valid Sv32 root page table that keeps the
/// caller's current execution reachable; `asid` must match the root and the
/// surrounding address-space switching discipline (currently always 0).
#[cfg(all(feature = "vm-mmu", target_arch = "riscv32"))]
pub unsafe fn activate(root_ppn: usize, asid: u16) {
    let satp = satp_value(root_ppn, asid);

    unsafe {
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
        core::arch::asm!(
            "csrw satp, {satp}",
            satp = in(reg) satp,
            options(nostack, preserves_flags),
        );
        core::arch::asm!("sfence.vma", options(nostack, preserves_flags));
    }
}
