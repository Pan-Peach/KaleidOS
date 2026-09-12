//! Core 派生、持有 provenance 的 MMIO 执行能力（Lease）。
//!
//! **Handle = authority；Lease = execution capability。** [`MmioLease`] 是
//! `MmioHandle` 经 Core 一次性校验后派生出的 scoped 访问能力：内部持有
//! `region.base` 的裸指针与长度，以及派生自的 `source` handle（slot +
//! generation）。受信 KernelNative 驱动可据此直接 volatile 访问，稳态不再
//! per-access 进 Core。
//!
//! # provenance 与撤销
//!
//! - 裸指针不可由组件凭空构造：只有 [`crate::handle::mmio::derive_lease`]
//!   能产出 lease，其 `source` 绑定具体的 handle 身份。
//! - **撤销是协作式的**：Core 撤销 / 释放 authority 后，此前已经派生出去的
//!   裸指针不会被追回（KernelNative 与 Core 共享同一地址空间、同特权级）。
//!   调用方必须在组件静默、相关使用结束后才认为 lease 失效（见
//!   `docs/driver-model.md` §3 / §7）。Sandboxed / Isolated 域由地址空间映射 +
//!   页表强制，撤销可真正切断访问。

use super::MmioHandle;
use super::dma::DmaHandle;

/// MMIO 执行能力。
///
/// 由 [`crate::handle::mmio::derive_lease`] 在 Core 校验
/// slot / generation / owner / 生命周期后派生；字段私有，组件不能自行构造。
#[derive(Clone, Copy, Debug)]
pub struct MmioLease {
    ptr: *mut u8,
    len: usize,
    source: MmioHandle,
}

impl MmioLease {
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

/// DMA 执行能力。
///
/// 由 [`crate::handle::dma::derive_lease`] 在 Core 校验 slot / generation /
/// owner / 生命周期后派生；字段私有，组件不能自行构造。`device_addr` 是设备
/// 可见地址——**v1 identity：等于 backing 物理基址**（无 IOMMU）。
#[derive(Clone, Copy, Debug)]
pub struct DmaLease {
    ptr: *mut u8,
    len: usize,
    device_addr: usize,
    source: DmaHandle,
}

impl DmaLease {
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
