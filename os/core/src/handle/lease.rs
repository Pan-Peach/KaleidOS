//! Core 派生、持有 provenance 的 MMIO/DMA **只读快照视图**（View）。
//!
//! **Handle = authority；View = 一次性校验后的裸指针 / 设备地址快照。**
//! [`MmioView`] / [`DmaView`] 由 Core 在一次性校验 handle 后派生，字段私有，
//! 组件不能自行构造：
//!
//! - 携带 `region.base` 裸指针（DMA 另有设备可见地址）以及派生自的 `source`
//!   handle（slot + generation）作为 provenance；
//! - 是 `Copy` 的**快照**：**不 pin 任何东西**——没有 `Drop`、不做引用计数、
//!   不阻止资源被 revoke / 释放 / 复用；
//! - **撤销是协作式的**：Core 撤销 / 释放 authority 后，此前已经派生出去的
//!   裸指针不会被追回（KernelNative 与 Core 共享同一地址空间、同特权级）。
//!   调用方必须在组件静默、相关使用结束后才认为 view 失效（见
//!   `docs/driver-model.md` §3 / §7）。Sandboxed / Isolated 域由地址空间映射 +
//!   页表强制，撤销可真正切断访问。
//!
//! **命名注意**：这两个 `Copy` 类型**不是** `MemoryLease`（`crate::memory` 中的
//! 唯一 RAII 分配属主，代表区域占用）；它们只是快照。旧名 `MmioLease` /
//! `DmaLease` 容易被误读成"持有/钉住资源"，故内部改名为 `MmioView` / `DmaView`
//! （见 `docs/resource-model-review.md` §C.3）。**导出名 `kcore_mmio_lease` /
//! `kcore_dma_lease` 是 ABI，保持不动。**

use super::MmioHandle;
use super::dma::DmaHandle;

/// MMIO 只读快照视图。
///
/// 由 [`crate::handle::mmio::derive_lease`] 在 Core 校验
/// slot / generation / owner / 生命周期后派生；字段私有，组件不能自行构造。
///
/// 它是 `Copy` 的裸指针（+ 长度）+ provenance（`source` handle）**快照**：
/// 不 pin、不阻止 revoke、撤销为协作式（已派生出去的裸指针不被追回）。
/// **不是** `MemoryLease`——后者才是 RAII 分配属主。
#[derive(Clone, Copy, Debug)]
pub struct MmioView {
    ptr: *mut u8,
    len: usize,
    source: MmioHandle,
}

impl MmioView {
    /// 仅供 Core 派生路径构造（字段私有，不对外暴露）。
    pub(crate) const fn new(ptr: *mut u8, len: usize, source: MmioHandle) -> Self {
        Self { ptr, len, source }
    }

    /// 映射区域基址指针（Core 从已校验的 region 派生）。
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// 映射区域字节长度。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 区域是否为空（`len == 0`）。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 派生自的 authority handle（provenance：slot + generation）。
    pub fn source(&self) -> MmioHandle {
        self.source
    }
}

/// DMA 只读快照视图。
///
/// 由 [`crate::handle::dma::derive_lease`] 在 Core 校验 slot / generation /
/// owner / 生命周期后派生；字段私有，组件不能自行构造。`device_addr` 是设备
/// 可见地址——**v1 identity：等于 backing 物理基址**（无 IOMMU）。
///
/// 与 [`MmioView`] 一样，它是 `Copy` 的裸指针 / 设备地址 + provenance **快照**：
/// 不 pin backing、不阻止 revoke、撤销为协作式；DMA backing 的物理回收是
/// quarantine 路径的职责，不在本类型。**不是** `MemoryLease`。
#[derive(Clone, Copy, Debug)]
pub struct DmaView {
    ptr: *mut u8,
    len: usize,
    device_addr: usize,
    source: DmaHandle,
}

impl DmaView {
    /// 仅供 Core 派生路径构造（字段私有，不对外暴露）。
    pub(crate) const fn new(
        ptr: *mut u8,
        len: usize,
        device_addr: usize,
        source: DmaHandle,
    ) -> Self {
        Self {
            ptr,
            len,
            device_addr,
            source,
        }
    }

    /// backing 区域基址指针（Core 从已校验的 region 派生）。
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// backing 区域字节长度。
    pub fn len(&self) -> usize {
        self.len
    }

    /// 区域是否为空（`len == 0`）。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 设备可见地址（v1 identity：与 [`Self::as_ptr`] 数值相同）。
    pub fn device_addr(&self) -> usize {
        self.device_addr
    }

    /// 派生自的 authority handle（provenance：slot + generation）。
    pub fn source(&self) -> DmaHandle {
        self.source
    }
}
