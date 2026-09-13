//! DMA authority 表（driver-model step 3）：Core 拥有的物理连续 DMA 区域。
//!
//! # 主线：request → authorize → grant → derive → use → quarantine → revoke
//!
//! 1. [`alloc`]：caller 必须**已经持有目标设备的 `MmioHandle`**；`device_index`
//!    由 Core 从该 handle 的 MMIO 表记录推导（`mmio::device_index_for`），
//!    **绝不接受组件自报的设备号**。Core 再分配一段物理连续区域
//!    （`memory::alloc_region`），把区域真相记进 `DmaRegion`，grant 出 `DmaHandle`。
//! 2. [`derive_lease`]：Core 一次性校验 handle 后派生 [`DmaLease`]——携带
//!    backing 指针、长度、**设备可见地址**与 `source`（provenance）。受信
//!    KernelNative 驱动据此直接读写。**v1 无 IOMMU：设备可见地址 == 物理基址
//!    （identity）**；映射由 Core 提交，组件不能自行指定设备地址。
//! 3. [`release`] / [`revoke_owner`]：把 `MemoryLease` 从 region 里**取出**、
//!    移入 Core 私有的 `QUARANTINE` 列表（**不释放**），再 revoke slot。
//!
//! # 撤销顺序（为什么是 quarantine，不是 free）
//!
//! ```text
//! 停止准入 → 设备静默（KernelNative 下不可强制）→ move 进 QUARANTINE → revoke
//! ```
//!
//! 设备可能仍在 DMA 往这块内存写；马上 free/复用会让设备写进已被重新分配的
//! 区域。Core 能撤销 authority（slot generation 前进 → Stale），但**不能确认
//! 设备已静默**（无 IOMMU，见 `docs/driver-model.md` §7/§11）。因此 backing
//! lease 只进 quarantine，不归还 buddy heap。**权威回收需要设备静默
//! （reset / 确认无未结清）——本版 deferred**，quarantine 是"不立即复用"的
//! 安全降级。
//!
//! # 测试指引（host 可验证）
//!
//! 直接往全局 MMIO 表 grant 一个带 `device_index` 的 `MmioRegion`，即可在 host
//! 上跑完 alloc → derive → release/revoke 全路径（不需要真实设备）。DMA 测试
//! 与其它碰全局堆的测试靠 `memory::test_support::GUARD` 串行化。

use super::lease::DmaLease;
use super::mmio::{self, MmioError};
use super::{Handle, HandleError, RequestContext, ResourceTable};
use crate::component::ComponentId;
use crate::irq::IrqSaveGuard;
use crate::memory::{self, MemoryError, MemoryLease};
use alloc::vec::Vec;
use spin::{Mutex, Once};

/// DMA 传输方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaDirection {
    /// 内存 → 设备。
    ToDevice,
    /// 设备 → 内存。
    FromDevice,
    /// 双向。
    Bidirectional,
}

impl DmaDirection {
    /// ABI 编码（`kcore_dma_alloc` 的 `direction` 参数）。
    ///
    /// **这是 Component ABI 的一部分**：`0 = ToDevice` / `1 = FromDevice` /
    /// `2 = Bidirectional`（见 `docs/driver-model.md` §6.2）。`kcomp-sdk` 镜像同一
    /// 组值；改动必须同步 `component/export.rs::dma_direction_from_i32`、本函数与
    /// SDK 的 `DmaDirection`（两侧各有锚定测试钉住 0/1/2）。
    pub const fn as_i32(self) -> i32 {
        match self {
            DmaDirection::ToDevice => 0,
            DmaDirection::FromDevice => 1,
            DmaDirection::Bidirectional => 2,
        }
    }
}

/// 一个 DMA 资源对象：一段 Core 拥有的物理连续内存。
///
/// `device_index` 从 caller 已持有的 `MmioHandle` 推导（不是组件自报）；
/// `device_addr` 是设备可见地址——**v1 identity：就是物理基址**（无 IOMMU）。
/// `lease` 是 backing 真相；release/revoke 时被 move 进 `QUARANTINE`，故 revoke
/// 后的 region drop 不会释放物理内存。
pub struct DmaRegion {
    pub device_index: u8,
    /// 实际 backing 容量（buddy region size，≥ 请求尺寸）。
    pub size: usize,
    pub direction: DmaDirection,
    /// 设备可见地址（v1 == 物理基址，identity）。
    pub device_addr: usize,
    /// backing 区域租约；`None` = 已 quarantine（或尚未 grant）。
    lease: Option<MemoryLease>,
}

/// Core 授予组件的 DMA authority。
pub type DmaHandle = Handle<DmaRegion>;

/// DMA 分配 / 派生 / 释放失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// handle 验证失败（slot/generation/owner/生命周期）。
    Handle(HandleError),
    /// 用于推导 `device_index` 的 `MmioHandle` 无效。
    Mmio(MmioError),
    /// 请求尺寸非法（0 / 超出分配器上限）。
    InvalidSize,
    /// 物理内存耗尽。
    Exhausted,
}

/// DMA 资源真相表。
pub struct DmaTable {
    table: ResourceTable<DmaRegion>,
}

impl DmaTable {
    pub const fn new() -> Self {
        Self {
            table: ResourceTable::new(),
        }
    }

    pub fn grant(&mut self, owner: ComponentId, region: DmaRegion) -> DmaHandle {
        self.table.grant(owner, region)
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得 DMA 对象。
    pub fn get(&self, caller: ComponentId, handle: DmaHandle) -> Result<&DmaRegion, HandleError> {
        self.table.get(caller, handle)
    }

    /// 释放：先把 backing lease 移入 `QUARANTINE`，再 revoke slot。
    ///
    /// 顺序不可颠倒——先 revoke 会让 region 被 drop，lease 当场 free（设备可能
    /// 仍在写）。取出后 slot 里的 region 已无 lease，revoke 时 drop 是空操作。
    pub fn release(&mut self, caller: ComponentId, handle: DmaHandle) -> Result<(), HandleError> {
        let region = self.table.get_mut(caller, handle)?;
        if let Some(lease) = region.lease.take() {
            quarantine(lease);
        }
        self.table.release(caller, handle)
    }

    /// 撤销 owner 全部 DMA authority；每个 backing lease 同样先 quarantine。
    pub fn revoke_owner(&mut self, owner: ComponentId) {
        for slot in self.table.slots_mut() {
            if slot.owner() != owner {
                continue;
            }
            if let Some(region) = slot.object_mut()
                && let Some(lease) = region.lease.take()
            {
                quarantine(lease);
            }
            slot.revoke();
        }
    }
}

impl Default for DmaTable {
    fn default() -> Self {
        Self::new()
    }
}

// —— 全局（`handle::init` 初始化；测试用 `DmaTable::new()`）——

static TABLE: Once<Mutex<DmaTable>> = Once::new();

/// Core 私有 quarantine：已撤销 authority 但设备可能仍在 DMA 的 backing 区域。
///
/// 只增不减：本版没有"确认设备静默"的手段，因此不归还、不复用。权威回收
/// （reset 设备后 free）是 deferred 项（见模块文档与 `docs/driver-model.md` §7）。
static QUARANTINE: Mutex<Vec<MemoryLease>> = Mutex::new(Vec::new());

fn quarantine(lease: MemoryLease) {
    QUARANTINE.lock().push(lease);
}

/// 已 quarantine 的区域数（测试断言"未释放"用）。
#[cfg(test)]
pub(crate) fn quarantine_len() -> usize {
    QUARANTINE.lock().len()
}

/// 初始化全局 DMA 表（`handle::init` 调用一次）。
pub fn init() {
    TABLE.call_once(|| Mutex::new(DmaTable::new()));
}

/// 取全局 DMA 表（init 后可用）。
pub fn get_table() -> &'static Mutex<DmaTable> {
    TABLE.get().expect("dma table not initialized")
}

/// 分配一段物理连续 DMA 区域并 grant `DmaHandle`。
///
/// `device_index` 由 `mmio` 从 caller 自己持有的 `MmioHandle` 推导（Core 真相），
/// 组件无法伪造设备身份；随后 `memory::alloc_region` 分配 backing。分配在
/// `IrqSaveGuard` 下进行（同 MMIO claim 路径），不跨 volatile 访问持锁。
pub fn alloc(
    ctx: &RequestContext,
    mmio: mmio::MmioHandle,
    size: usize,
    dir: DmaDirection,
) -> Result<DmaHandle, DmaError> {
    // device_index 只能来自 caller 已持有的 MMIO authority。
    let device_index = mmio::device_index_for(ctx, mmio).map_err(DmaError::Mmio)?;

    let _guard = IrqSaveGuard::new();
    let lease = memory::alloc_region(size).map_err(|error| match error {
        MemoryError::Exhausted => DmaError::Exhausted,
        MemoryError::InvalidSize => DmaError::InvalidSize,
        // alloc_region 不会返回 DoubleFree（那是 free 路径的错误）。
        MemoryError::DoubleFree => DmaError::InvalidSize,
    })?;
    let device_addr = lease.base();
    let capacity = lease.size();
    Ok(get_table().lock().grant(
        ctx.component,
        DmaRegion {
            device_index,
            size: capacity,
            direction: dir,
            device_addr,
            lease: Some(lease),
        },
    ))
}

/// 派生 [`DmaLease`]：Core 校验一次 handle，返回 backing `(ptr, len)` +
/// **设备可见地址** + `source` handle（KernelNative 直接 DMA 快路径）。
///
/// 撤销是协作式的：release/revoke 之前派生出去的指针不会被追回；且 backing
/// 只进 quarantine（见模块文档）。
pub fn derive_lease(ctx: &RequestContext, h: DmaHandle) -> Result<DmaLease, DmaError> {
    let _guard = IrqSaveGuard::new();
    let table = get_table().lock();
    let region = table.get(ctx.component, h).map_err(DmaError::Handle)?;
    let lease = region
        .lease
        .as_ref()
        .ok_or(DmaError::Handle(HandleError::Revoked))?;
    Ok(DmaLease::new(
        lease.base() as *mut u8,
        region.size,
        region.device_addr,
        h,
    ))
}

/// 主动释放 DMA authority；backing lease 进 `QUARANTINE`（不 free）。
pub fn release(ctx: &RequestContext, h: DmaHandle) -> Result<(), DmaError> {
    let _guard = IrqSaveGuard::new();
    get_table()
        .lock()
        .release(ctx.component, h)
        .map_err(DmaError::Handle)
}

/// 撤销 owner 持有的全部 DMA authority；backing lease 进 `QUARANTINE`（不 free）。
pub fn revoke_owner(owner: ComponentId) {
    let _guard = IrqSaveGuard::new();
    get_table().lock().revoke_owner(owner);
}

#[cfg(test)]
mod tests {
    use super::{DmaDirection, DmaError, alloc, derive_lease, get_table, quarantine_len, release};
    use crate::component::ComponentId;
    use crate::handle::mmio::{self, MmioError, MmioRegion};
    use crate::handle::{HandleError, RequestContext};
    use crate::memory::test_support;

    fn context(component: ComponentId) -> RequestContext {
        RequestContext {
            component,
            task: None,
        }
    }

    /// 往全局 MMIO 表 grant 一个带 `device_index` 的 handle（host：不需要真实设备）。
    fn grant_mmio(owner: ComponentId, device_index: u8) -> mmio::MmioHandle {
        mmio::get_table().lock().grant(
            owner,
            MmioRegion {
                base: 0x1000_0000 + device_index as usize * 0x1000,
                size: 0x1000,
                device_index,
            },
        )
    }

    /// alloc 从 caller 的 MmioHandle 绑定 device_index，并给出连续 backing：
    /// `device_addr == backing base`（identity）、`size == backing capacity`。
    #[test]
    fn alloc_binds_device_index_and_returns_contiguous_region() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();
        crate::handle::init();

        let owner = ComponentId::from_raw(40);
        let ctx = context(owner);
        let mmio = grant_mmio(owner, 5);

        let handle = alloc(&ctx, mmio, 8192, DmaDirection::ToDevice).expect("alloc");
        {
            let table = get_table().lock();
            let region = table.get(owner, handle).unwrap();
            let lease = region.lease.as_ref().expect("backing lease");
            assert_eq!(region.device_index, 5, "device_index 必须来自 MmioHandle");
            assert_eq!(region.direction, DmaDirection::ToDevice);
            assert_eq!(region.device_addr, lease.base(), "v1 identity");
            assert_eq!(region.size, lease.size());
            assert!(region.size >= 8192, "buddy 容量不小于请求");
            assert_eq!(region.size % crate::memory::ALLOC_GRANULE, 0);
        }
        assert_eq!(release(&ctx, handle), Ok(()));
    }

    /// derive_lease 携带 ptr / len / device_addr / source，且 ptr == device_addr。
    #[test]
    fn derive_lease_carries_ptr_len_device_addr_and_source() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();
        crate::handle::init();

        let owner = ComponentId::from_raw(41);
        let ctx = context(owner);
        let mmio = grant_mmio(owner, 6);
        let handle = alloc(&ctx, mmio, 4096, DmaDirection::Bidirectional).expect("alloc");

        let (expected_addr, expected_len) = {
            let table = get_table().lock();
            let region = table.get(owner, handle).unwrap();
            (region.device_addr, region.size)
        };

        let lease = derive_lease(&ctx, handle).unwrap();
        assert!(!lease.as_ptr().is_null());
        assert!(!lease.is_empty());
        assert_eq!(
            lease.as_ptr() as usize,
            expected_addr,
            "identity: ptr == device_addr"
        );
        assert_eq!(lease.len(), expected_len);
        assert_eq!(lease.device_addr(), expected_addr);
        assert_eq!(lease.source(), handle);

        assert_eq!(release(&ctx, handle), Ok(()));
    }

    /// 对抗：wrong owner 的 MmioHandle 不能用于分配（设备身份不可借用）。
    #[test]
    fn alloc_rejects_wrong_owner_mmio_handle() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();
        crate::handle::init();

        let owner = ComponentId::from_raw(42);
        let other = ComponentId::from_raw(43);
        let mmio = grant_mmio(owner, 7);

        assert_eq!(
            alloc(&context(other), mmio, 4096, DmaDirection::ToDevice).unwrap_err(),
            DmaError::Mmio(MmioError::Handle(HandleError::WrongOwner))
        );
    }

    /// 对抗：size == 0 拒绝。
    #[test]
    fn alloc_rejects_invalid_size() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();
        crate::handle::init();

        let owner = ComponentId::from_raw(44);
        let ctx = context(owner);
        let mmio = grant_mmio(owner, 8);

        assert_eq!(
            alloc(&ctx, mmio, 0, DmaDirection::FromDevice).unwrap_err(),
            DmaError::InvalidSize
        );
    }

    /// release：handle 变 Stale，且 backing lease 进 QUARANTINE（未释放）。
    #[test]
    fn release_quarantines_lease_and_stales_handle() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();
        crate::handle::init();

        let owner = ComponentId::from_raw(45);
        let other = ComponentId::from_raw(46);
        let ctx = context(owner);
        let mmio = grant_mmio(owner, 9);
        let handle = alloc(&ctx, mmio, 4096, DmaDirection::ToDevice).expect("alloc");

        let before = quarantine_len();

        // wrong owner 不能释放
        assert_eq!(
            release(&context(other), handle),
            Err(DmaError::Handle(HandleError::WrongOwner))
        );

        assert_eq!(release(&ctx, handle), Ok(()));
        assert_eq!(
            quarantine_len(),
            before + 1,
            "lease 必须被 quarantine，不能 free"
        );
        assert!(matches!(
            get_table().lock().get(owner, handle),
            Err(HandleError::Stale)
        ));
        // 重复释放 → Stale
        assert_eq!(
            release(&ctx, handle),
            Err(DmaError::Handle(HandleError::Stale))
        );
        // stale 后不能再派生 lease
        assert_eq!(
            derive_lease(&ctx, handle).unwrap_err(),
            DmaError::Handle(HandleError::Stale)
        );
    }

    /// revoke_owner：全部 handle 变 Stale，backing lease 进 QUARANTINE（未释放）。
    #[test]
    fn revoke_owner_quarantines_and_stales_handles() {
        let _heap = test_support::GUARD.lock();
        test_support::ensure_init();
        crate::handle::init();

        let owner = ComponentId::from_raw(47);
        let ctx = context(owner);
        let first = grant_mmio(owner, 10);
        let second = grant_mmio(owner, 11);
        let h1 = alloc(&ctx, first, 4096, DmaDirection::ToDevice).expect("alloc 1");
        let h2 = alloc(&ctx, second, 4096, DmaDirection::FromDevice).expect("alloc 2");

        let before = quarantine_len();
        get_table().lock().revoke_owner(owner);
        assert_eq!(
            quarantine_len(),
            before + 2,
            "两条 backing lease 都必须 quarantine"
        );
        assert!(matches!(
            get_table().lock().get(owner, h1),
            Err(HandleError::Stale)
        ));
        assert!(matches!(
            get_table().lock().get(owner, h2),
            Err(HandleError::Stale)
        ));
        assert_eq!(
            derive_lease(&ctx, h1).unwrap_err(),
            DmaError::Handle(HandleError::Stale)
        );
    }
}
