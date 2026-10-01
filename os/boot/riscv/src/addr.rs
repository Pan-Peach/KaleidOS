//! boot 本地的链接地址归一化（RV32 boot 与两个 XLEN 的 selftest 共用）。
//!
//! **这是 boot 的镜像 VMA/LMA 换算，不是可复用的 VA→PA 原语。** RV64 把内核
//! 链接在高半区、装载在低物理地址：链接地址先归一到低别名（early identity
//! 视图），boot selftest 才能按物理地址读回镜像字节 / poke 页表。RV32 全程
//! identity（VA == PA）。运行期翻译属于映射所有者
//! （`AddressSpaceBackend::translate`），不从这里导出。

#[cfg(target_arch = "riscv64")]
pub(crate) fn linked_to_physical(address: usize) -> usize {
    crate::vm::bootstrap::physical_address_of(address)
}

#[cfg(target_arch = "riscv32")]
pub(crate) fn linked_to_physical(address: usize) -> usize {
    address
}
