//! NoMMU 恒等翻译 backend（**骨架**：结构/契约就位，语义实现由人类完成）。
//!
//! 这是 `AddressSpaceBackend`（arch/src/vm.rs）在无 MMU 目标上的诚实实现：
//! 没有页表、没有 page fault、没有 satp、VA≈PA。不伪装成 Sv32。
//!
//! 关键点：
//! - `GRANULE = 1`：Core 的地址空间对齐校验（`KernelAddressSpace::validate`）
//!   使用 `B::GRANULE` 做掩码，`1 - 1 == 0` → 任何对齐都合法，校验自动退化为
//!   no-op —— **Core 不需要为 NoMMU 写任何条件编译**（对应验收：core 侧测试
//!   `core_validation_accepts_unaligned_with_granule_one`）。
//! - map 是 identity 一致性校验（VA 区间必须与 PA 区间重合），translate 恒等，
//!   activate 无 satp 可写（no-op）。
//!
//! TODO(实现)：
//! 1. `map`：校验 `va == pa && va.size == pa.size`，不做任何硬件操作；
//! 2. `unmap`：恒等下无 TLB 可刷，no-op（或按未来 profile 记 ledger）；
//! 3. `translate`：恒等返回 `va`；
//! 4. `activate`：no-op。
//!
//! 实现后启用 `tests` 里的 `#[ignore]` 骨架测试。

use crate::vm::{AddressSpaceBackend, MappingPermission, PhysicalRange, VirtualRange};

/// NoMMU backend 的错误：恒等约束失败（VA 与 PA 不重合）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoMmuError {
    IdentityMismatch,
}

/// 恒等翻译 backend：一个 marker，无状态（无页表、无帧、无 ASID）。
pub struct NoMmuAddressSpace;

impl AddressSpaceBackend for NoMmuAddressSpace {
    /// 恒等：任何对齐都合法（Core 校验自动 no-op）。
    const GRANULE: usize = 1;

    type Error = NoMmuError;

    fn map(
        &mut self,
        _va: VirtualRange,
        _pa: PhysicalRange,
        _perm: MappingPermission,
    ) -> Result<(), NoMmuError> {
        todo!("NoMmuAddressSpace::map —— identity 一致性校验后 no-op")
    }

    fn unmap(&mut self, _va: VirtualRange) -> Result<(), NoMmuError> {
        todo!("NoMmuAddressSpace::unmap —— no-op")
    }

    fn translate(&self, _va: usize) -> Option<usize> {
        todo!("NoMmuAddressSpace::translate —— 恒等返回 va")
    }

    fn activate(&self) -> Result<(), NoMmuError> {
        todo!("NoMmuAddressSpace::activate —— 无 satp，no-op")
    }
}

#[cfg(test)]
mod tests {
    //! 骨架测试：语义实现后逐个去掉 `#[ignore]` 即可验收
    //! （"Core 不依赖 MMU"的跨 crate 验收在 kernel 侧：
    //! `address_space::tests::core_validation_accepts_unaligned_with_granule_one`）。

    #[test]
    #[ignore = "NoMmuAddressSpace 语义实现后启用"]
    fn identity_map_translate_roundtrip() {
        let _space = super::NoMmuAddressSpace;
        let va = super::VirtualRange {
            base: 0x2000_0000,
            size: 0x1000,
        };
        // TODO: map(va, va, READ|WRITE) → Ok；translate(va.base) == Some(va.base)；
        //       unmap(va) 后 translate == None。
        let _ = va;
    }

    #[test]
    #[ignore = "NoMmuAddressSpace 语义实现后启用"]
    fn identity_mismatch_is_rejected() {
        let _space = super::NoMmuAddressSpace;
        // TODO: map(va = 0x2000, pa = 0x3000) → Err(NoMmuError::IdentityMismatch)。
    }
}
