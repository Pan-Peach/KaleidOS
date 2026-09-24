//! NoMMU 恒等翻译 backend。
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

    /// NoMMU 没有页表、没有 satp：`AddressSpaceBackend` 可用**不等于**能承载
    /// Isolated 域。Core 的 Isolated 部署 / 装载路径据此显式拒绝。
    const PRIVATE_ADDRESS_SPACE: bool = false;

    /// 没有可切换的翻译状态：描述符是空类型（汇编转换在 NoMMU 上不存在）。
    type Activation = ();

    type Error = NoMmuError;

    fn create(alloc: crate::vm::PageAlloc) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        // NoMMU 不需要页表分配器。
        let _ = alloc;
        Ok(Self)
    }

    fn map(
        &mut self,
        va: VirtualRange,
        pa: PhysicalRange,
        _perm: MappingPermission,
    ) -> Result<(), NoMmuError> {
        if va.base != pa.base || va.size != pa.size {
            return Err(NoMmuError::IdentityMismatch);
        }

        Ok(())
    }

    fn unmap(&mut self, _va: VirtualRange) -> Result<(), NoMmuError> {
        Ok(())
    }

    fn translate(&self, va: usize) -> Option<usize> {
        Some(va)
    }

    fn activate(&self) -> Result<(), NoMmuError> {
        Ok(())
    }

    fn prepare_activation(&self) {}
}

#[cfg(test)]
mod tests {
    //! （"Core 不依赖 MMU"的跨 crate 验收在 kernel 侧：
    //! `address_space::tests::core_validation_accepts_unaligned_with_granule_one`）。

    use crate::vm::AddressSpaceBackend;

    #[test]
    fn identity_map_translate_roundtrip() {
        let mut space = super::NoMmuAddressSpace;
        let va = super::VirtualRange {
            base: 0x2000_0000,
            size: 0x1000,
        };
        assert_eq!(
            space.map(
                va,
                super::PhysicalRange {
                    base: va.base,
                    size: va.size,
                },
                super::MappingPermission::READ | super::MappingPermission::WRITE,
            ),
            Ok(())
        );
        assert_eq!(space.translate(va.base + 7), Some(va.base + 7));
        assert_eq!(space.unmap(va), Ok(()));
        // NoMMU has no mapping ledger in the backend; Core owns that truth.
        assert_eq!(space.translate(va.base + 7), Some(va.base + 7));
    }

    #[test]
    fn identity_mismatch_is_rejected() {
        let mut space = super::NoMmuAddressSpace;
        let va = super::VirtualRange {
            base: 0x2000,
            size: 0x1000,
        };
        assert_eq!(
            space.map(
                va,
                super::PhysicalRange {
                    base: 0x3000,
                    size: va.size,
                },
                super::MappingPermission::READ,
            ),
            Err(super::NoMmuError::IdentityMismatch)
        );
    }
}
