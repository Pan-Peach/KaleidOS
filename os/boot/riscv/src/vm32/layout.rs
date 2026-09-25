//! RV32 镜像布局：linker32.ld 的唯一解释者。
//!
//! RV32 profile 是 identity 视图（`VA == PA`），没有高半区；段权限与 RV64 同一套
//! 语义（text RX / rodata R / data RW / bss RW），只是 VA 就是物理地址。
//!
//! ```text
//! .text.entry,.text        RX
//! .rodata.entry,.rodata,.initpkg  R
//! .data                    RW
//! .bss.stack,.bss          RW (NOLOAD)
//! ```

use arch::vm::MappingPermission;

/// 一个连续的已链接镜像段（identity VA 范围 + 权限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelSection32 {
    pub va_start: usize,
    pub va_end: usize,
    pub permission: MappingPermission,
}

/// RV32 长期 root 的段布局（boot 与 runtime 的共同输入）。
#[derive(Debug, Clone, Copy)]
pub struct KernelLayout32 {
    pub text: KernelSection32,
    pub rodata: KernelSection32,
    pub data: KernelSection32,
    pub bss: KernelSection32,
}

impl KernelLayout32 {
    /// 顺序输出全部段（runtime builder 遍历）。
    pub fn sections(&self) -> [KernelSection32; 4] {
        [self.text, self.rodata, self.data, self.bss]
    }
}

// 链接脚本符号（段 VMA 范围，identity = 物理地址）。
unsafe extern "C" {
    static __text_vma_start: u8;
    static __text_vma_end: u8;
    static __rodata_vma_start: u8;
    static __rodata_vma_end: u8;
    static __data_vma_start: u8;
    static __data_vma_end: u8;
    static __bss_vma_start: u8;
    static __bss_vma_end: u8;
}

fn linker_addr(symbol: *const u8) -> usize {
    symbol as usize
}

/// 从 linker symbols 构造当前 RV32 镜像的 `KernelLayout32`（唯一解释者）。
pub fn kernel_layout32() -> KernelLayout32 {
    let rw = MappingPermission::READ | MappingPermission::WRITE;
    KernelLayout32 {
        text: KernelSection32 {
            va_start: linker_addr(core::ptr::addr_of!(__text_vma_start)),
            va_end: linker_addr(core::ptr::addr_of!(__text_vma_end)),
            permission: MappingPermission::READ | MappingPermission::EXECUTE,
        },
        rodata: KernelSection32 {
            va_start: linker_addr(core::ptr::addr_of!(__rodata_vma_start)),
            va_end: linker_addr(core::ptr::addr_of!(__rodata_vma_end)),
            permission: MappingPermission::READ,
        },
        data: KernelSection32 {
            va_start: linker_addr(core::ptr::addr_of!(__data_vma_start)),
            va_end: linker_addr(core::ptr::addr_of!(__data_vma_end)),
            permission: rw,
        },
        bss: KernelSection32 {
            va_start: linker_addr(core::ptr::addr_of!(__bss_vma_start)),
            va_end: linker_addr(core::ptr::addr_of!(__bss_vma_end)),
            permission: rw,
        },
    }
}
