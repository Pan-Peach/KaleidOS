//! 内核镜像布局 —— boot 与 runtime 两张页表的**共同输入**。
//!
//! ```text
//! linker.ld → kernel_layout() → KernelLayout
//!                                    ├───────────┐
//!                                    ↓           ↓
//!                           vm::bootstrap   vm::runtime
//!                           early/static   dynamic/buddy
//! ```
//!
//! linker symbols 只在**这里**解释一次：boot 与 runtime 不会各自读一遍，
//! 避免两处对段范围/权限的理解漂移。两张页表的权限保证来源一致：
//!
//! ```text
//! .text      永远 RX
//! .rodata    永远 R
//! .initpkg   永远 R（只读归档，与 .rodata 同级）
//! .data      永远 RW
//! .bss       永远 RW
//! ```
//!
//! 差异只在于实现时机与 backing（静态池 vs buddy 动态页表）。
//!
//! `KernelSection.flags` 使用 arch 的 `PteFlags`；权限语义与 arch backend 一致。

use arch::riscv::mmu::sv39::PteFlags;

/// 内核链接虚拟基址（高半区）。layout 与 bootstrap 共用同一锚点。
pub const KERNEL_VMA: usize = 0xffff_ffc0_8020_0000;
/// 高半区偏移（identity ↔ 高别名 换算）。
///
/// **boot 本地布局常量**（由 `linker.ld` 决定），不是对 arch 公共契约的固定
/// 偏移承诺；运行期 VA→PA 一律走映射所有者
/// （`AddressSpaceBackend::translate`），不从本常量推导。
pub const HIGH_HALF_OFFSET: usize = 0xffff_ffc0_0000_0000;

/// Permission set for the executable text segment: read + execute.
pub const KERNEL_TEXT_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::X)
    .union(PteFlags::A)
    .union(PteFlags::D);
/// Permission set for read-only data (`.rodata`, embedded `.initpkg`).
pub const KERNEL_RODATA_FLAGS: PteFlags = PteFlags::R.union(PteFlags::A).union(PteFlags::D);
/// Permission set for writable data (`.data`, `.bss`).
pub const KERNEL_DATA_FLAGS: PteFlags = PteFlags::R
    .union(PteFlags::W)
    .union(PteFlags::A)
    .union(PteFlags::D);

/// One contiguous linked-image run mapped with a single permission set.
///
/// `va_start`/`va_end` are high-half virtual addresses (exclusive end).  The
/// physical address of each page is derived from the runtime kernel PA and the
/// offset from the link-time kernel VMA (`KERNEL_VMA`), so the boot/runtime
/// builders only need the linker section ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelSection {
    pub va_start: usize,
    pub va_end: usize,
    pub flags: PteFlags,
}

/// 内核镜像的分段布局（boot/runtime 共同消费）。
#[derive(Debug, Clone, Copy)]
pub struct KernelLayout {
    pub text: KernelSection,
    pub rodata: KernelSection,
    pub initpkg: KernelSection,
    pub data: KernelSection,
    pub bss: KernelSection,
}

impl KernelLayout {
    /// 顺序输出全部段（供 `bootstrap::init` / `runtime::build` 遍历）。
    pub fn sections(&self) -> [KernelSection; 5] {
        [self.text, self.rodata, self.initpkg, self.data, self.bss]
    }
}

// 链接脚本符号（段 VMA 范围）。仅在 layout 模块声明一次；
// main64 需要的 `__bootstrap_start/__bootstrap_end`（镜像 PA 范围）仍在 main64。
unsafe extern "C" {
    static __text_vma_start: u8;
    static __text_vma_end: u8;
    static __rodata_vma_start: u8;
    static __rodata_vma_end: u8;
    static __initpkg_start: u8;
    static __initpkg_end: u8;
    static __data_vma_start: u8;
    static __data_vma_end: u8;
    static __bss_vma_start: u8;
    static __bss_vma_end: u8;
}

fn linker_addr(symbol: *const u8) -> usize {
    symbol as usize
}

/// 从 linker symbols 构造当前镜像的 KernelLayout（唯一解释者）。
pub fn kernel_layout() -> KernelLayout {
    KernelLayout {
        text: KernelSection {
            va_start: linker_addr(core::ptr::addr_of!(__text_vma_start)),
            va_end: linker_addr(core::ptr::addr_of!(__text_vma_end)),
            flags: KERNEL_TEXT_FLAGS,
        },
        rodata: KernelSection {
            va_start: linker_addr(core::ptr::addr_of!(__rodata_vma_start)),
            va_end: linker_addr(core::ptr::addr_of!(__rodata_vma_end)),
            flags: KERNEL_RODATA_FLAGS,
        },
        initpkg: KernelSection {
            va_start: linker_addr(core::ptr::addr_of!(__initpkg_start)),
            va_end: linker_addr(core::ptr::addr_of!(__initpkg_end)),
            flags: KERNEL_RODATA_FLAGS,
        },
        data: KernelSection {
            va_start: linker_addr(core::ptr::addr_of!(__data_vma_start)),
            va_end: linker_addr(core::ptr::addr_of!(__data_vma_end)),
            flags: KERNEL_DATA_FLAGS,
        },
        bss: KernelSection {
            va_start: linker_addr(core::ptr::addr_of!(__bss_vma_start)),
            va_end: linker_addr(core::ptr::addr_of!(__bss_vma_end)),
            flags: KERNEL_DATA_FLAGS,
        },
    }
}
