//! 新 ISA 的**显式占位**地址空间（骨架）。
//!
//! 它不是 Sv39、也不是 NoMMU：`PRIVATE_ADDRESS_SPACE = false`，所有方法
//! `todo!()`。存在的意义是让新 ISA 在“页表后端尚未实现”时仍能把 HAL 接口立住，
//! 而不是偷偷借用别的 ISA 的页表语义。新 ISA bring-up 时，把
//! [`crate::AddressSpaceImpl`] 原地替换成本 ISA 的真实页表后端，并删除本文件。

use crate::vm::{AddressSpaceBackend, MappingPermission, PageAlloc, PhysicalRange, VirtualRange};

/// 占位地址空间；没有任何真实翻译能力。
#[allow(dead_code)]
pub struct StubAddressSpace {
    _private: (),
}

/// 占位后端的错误类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StubVmError {
    /// 该后端尚未实现。
    Unsupported,
}

impl AddressSpaceBackend for StubAddressSpace {
    /// 4 KiB 是保守占位；实现时按 ISA 页大小设定。
    const GRANULE: usize = 4096;
    /// 尚未提供私有地址空间能力。
    const PRIVATE_ADDRESS_SPACE: bool = false;

    type Activation = ();
    type Error = StubVmError;

    fn create(_alloc: PageAlloc) -> Result<Self, Self::Error> {
        todo!("new ISA: build the page-table root")
    }

    fn map(
        &mut self,
        _va: VirtualRange,
        _pa: PhysicalRange,
        _perm: MappingPermission,
    ) -> Result<(), Self::Error> {
        todo!("new ISA: install a leaf mapping")
    }

    fn unmap(&mut self, _va: VirtualRange) -> Result<(), Self::Error> {
        todo!("new ISA: remove a mapping")
    }

    fn translate(&self, _va: usize) -> Option<usize> {
        todo!("new ISA: walk the page table")
    }

    fn activate(&self) -> Result<(), Self::Error> {
        todo!("new ISA: switch the active page table")
    }

    fn prepare_activation(&self) -> Self::Activation {
        todo!("new ISA: package the activation descriptor")
    }
}
