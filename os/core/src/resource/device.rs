//! Device ownership：Core 记"哪台设备归哪个 Component"，并给出本执行域下的可访问窗口。
//!
//! # 模型
//!
//! ```text
//! MachineInfo.devices（启动期静态发现）
//!   │  kcore_device_nth(compatible, ordinal)  → DeviceId（纯身份，不是 authority）
//!   ▼
//! kcore_device_claim(device_id)
//!   │  Core: 解析设备记录 → 独占检查 → 记 owner → 解析本域窗口
//!   ▼
//! (mmio: *mut u8, mmio_len)      ← KernelNative：直接寄存器基址
//!                                  Isolated：映射进组件地址空间后的 VA
//! ```
//!
//! claim 之后 driver 自己 `volatile` 读写；steady state 不再进 Core。
//!
//! # 这里只保留真实的 correctness
//!
//! - **独占**：一台设备最多一个 owner（没这个，两个驱动会同时写同一台设备）。
//! - **quarantine**：组件失败 ≠ 设备已静默。失败后该设备保持不可认领直到
//!   reboot（不做 reset/recovery 框架）；优雅 release 不 quarantine，设备可复用。
//!
//! 不做 per-access 鉴权——KernelNative 是可信代码（见模块 `resource` 文档）。

use super::{RequestContext, ResourceKind};
use crate::component::ComponentId;
use crate::irq::IrqSaveGuard;
use crate::machine::{self, DeviceId, IoSpace};
use crate::trace::{TraceEvent, emit};
use alloc::boxed::Box;
use alloc::vec;
use spin::{Mutex, Once};

/// 本执行域下的 device MMIO 窗口。
///
/// `mmio` 是 Core 解析出的可直接访问地址：KernelNative 下就是寄存器基址
/// （identity）；Isolated 下是映射进组件地址空间的 VA。两者对 driver 同形。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DeviceMapping {
    pub mmio: *mut u8,
    pub mmio_len: usize,
}

impl core::fmt::Debug for DeviceMapping {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeviceMapping")
            .field("mmio", &self.mmio)
            .field("mmio_len", &self.mmio_len)
            .finish()
    }
}

/// 认领失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClaimError {
    /// `DeviceId` 越界，或机器信息尚未提交（正常组件运行期不可达）。
    DeviceNotFound,
    /// 该设备不是 MMIO（PIO 空间；当前不支持）。
    NotMmio,
    /// 该设备已被认领，或已被失败 quarantine 标记。
    DeviceBusy,
}

/// 释放失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceReleaseError {
    /// `DeviceId` 越界 / 机器信息未提交 / 该设备当前无人认领。
    DeviceNotFound,
    /// caller 不是该设备的 owner。
    NotOwner,
    /// 该设备仍有 live IRQ route / DMA mapping（拆机顺序：先静默并释放子项）。
    HasChildren,
}

/// 每个设备槽位的 ownership 真相。
#[derive(Clone, Copy)]
struct DeviceSlot {
    owner: Option<ComponentId>,
    quarantine: bool,
}

impl DeviceSlot {
    const EMPTY: Self = Self {
        owner: None,
        quarantine: false,
    };
}

/// Device ownership 真相表：**按已提交快照的设备数定容**的 boxed slice。
///
/// 长度即容量（`MachineInfo.devices.len()`）：没有 256 项固定表，没有 u8 收窄。
/// `DeviceId` 的完整 `u32` 空间由 `core::init` 校验可索引；`claim` / `release`
/// 对越界 id 返回 `DeviceNotFound`（不 panic）。空设备表合法。
pub struct DeviceTable {
    slots: Box<[DeviceSlot]>,
}

impl DeviceTable {
    /// 建立恰好 `devices` 个空槽位的表（快照设备数）。
    pub fn new(devices: usize) -> Self {
        Self {
            slots: vec![DeviceSlot::EMPTY; devices].into_boxed_slice(),
        }
    }

    /// 槽位数 = 已提交快照的设备数。
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn slot(&self, device: DeviceId) -> Option<&DeviceSlot> {
        // raw() 是 u32；64 位目标上 as usize 是无损加宽，32 位目标上同宽。
        self.slots.get(device.raw() as usize)
    }

    fn slot_mut(&mut self, device: DeviceId) -> Option<&mut DeviceSlot> {
        self.slots.get_mut(device.raw() as usize)
    }

    /// 该设备的 owner（`None` = 空闲 / `DeviceId` 越界）。
    pub fn owner(&self, device: DeviceId) -> Option<ComponentId> {
        self.slot(device).and_then(|slot| slot.owner)
    }

    /// 该设备是否处于失败 quarantine（保持到 reboot）。
    pub fn is_quarantined(&self, device: DeviceId) -> bool {
        self.slot(device).is_some_and(|slot| slot.quarantine)
    }

    /// 独占认领：越界 → [`DeviceClaimError::DeviceNotFound`]；
    /// 已认领或已 quarantine → [`DeviceClaimError::DeviceBusy`]。
    pub fn claim(
        &mut self,
        component: ComponentId,
        device: DeviceId,
    ) -> Result<(), DeviceClaimError> {
        let Some(slot) = self.slot_mut(device) else {
            return Err(DeviceClaimError::DeviceNotFound);
        };
        if slot.quarantine || slot.owner.is_some() {
            return Err(DeviceClaimError::DeviceBusy);
        }
        slot.owner = Some(component);
        Ok(())
    }

    /// 主动释放：仅 owner 本人可释放；越界视为不存在。
    pub fn release(
        &mut self,
        component: ComponentId,
        device: DeviceId,
    ) -> Result<(), DeviceReleaseError> {
        let Some(slot) = self.slot_mut(device) else {
            return Err(DeviceReleaseError::DeviceNotFound);
        };
        match slot.owner {
            Some(owner) if owner == component => {
                slot.owner = None;
                Ok(())
            }
            Some(_) => Err(DeviceReleaseError::NotOwner),
            None => Err(DeviceReleaseError::DeviceNotFound),
        }
    }

    /// 失败路径：撤销 component 的全部 device，并 quarantine（不立即复用）。
    pub fn quarantine_owner(&mut self, component: ComponentId) {
        for slot in self.slots.iter_mut() {
            if slot.owner == Some(component) {
                slot.owner = None;
                slot.quarantine = true;
            }
        }
    }

    /// 清空失败 quarantine（**仅测试**：进程全局表不能在用例间回退）。
    #[cfg(test)]
    pub(crate) fn clear_quarantine(&mut self) {
        for slot in self.slots.iter_mut() {
            slot.quarantine = false;
        }
    }
}

// —— 全局（`resource::init` 初始化；测试用 `DeviceTable::new()`）——

static TABLE: Once<Mutex<DeviceTable>> = Once::new();

/// 测试专用的全局表覆盖（见 [`install_for_test`]）。
#[cfg(test)]
static TEST_TABLE: Mutex<Option<&'static Mutex<DeviceTable>>> = Mutex::new(None);

/// 初始化全局 device 表（`resource::init` 调用一次）：按已提交快照的设备数定容。
pub fn init() {
    let devices = crate::machine::committed().map_or(0, |info| info.devices.len());
    TABLE.call_once(|| Mutex::new(DeviceTable::new(devices)));
}

/// **仅测试**：用恰好 `devices` 个槽位的全新表覆盖全局读路径（测试用例各自
/// 带自己的机器 fixture；进程全局 `Once` 不能按用例重定容）。
#[cfg(test)]
pub(crate) fn install_for_test(devices: usize) {
    let table: &'static Mutex<DeviceTable> =
        Box::leak(Box::new(Mutex::new(DeviceTable::new(devices))));
    *TEST_TABLE.lock() = Some(table);
}

/// 取全局 device 表（init 后可用）。
pub fn get_table() -> &'static Mutex<DeviceTable> {
    #[cfg(test)]
    if let Some(table) = *TEST_TABLE.lock() {
        return table;
    }
    TABLE.get().expect("device table not initialized")
}

/// 该设备的 owner（跨模块查询：IRQ / DMA 的归属验证）。
pub fn owner_of(device: DeviceId) -> Option<ComponentId> {
    get_table().lock().owner(device)
}

/// 认领**一台确切设备**：resolve → 独占检查 → 记 owner → 解析本域窗口。
///
/// `device` 由发现阶段 [`crate::machine::nth_compatible`] 给出；`DeviceId` 本身
/// 不授予任何东西——authority 从 Core 记录 owner 开始。
pub fn claim(ctx: &RequestContext, device: DeviceId) -> Result<DeviceMapping, DeviceClaimError> {
    let Some(machine) = machine::committed() else {
        return Err(DeviceClaimError::DeviceNotFound);
    };
    let Some(descriptor) = machine.devices.get(device.raw() as usize) else {
        return Err(DeviceClaimError::DeviceNotFound);
    };
    // 只看 MMIO 空间（PIO 设备当前不认领）。
    let IoSpace::Mmio { base, size } = descriptor.space else {
        return Err(DeviceClaimError::NotMmio);
    };

    let _guard = IrqSaveGuard::new();
    get_table().lock().claim(ctx.component, device)?;
    emit(TraceEvent::ResourceGrant {
        component: ctx.component,
        kind: ResourceKind::Device,
        id: u64::from(device.raw()),
    });
    Ok(resolve_mapping(base, size))
}

/// 解析本执行域下的 device 窗口。
///
/// - **KernelNative**（当前唯一域）：identity，直接返回寄存器基址。
/// - **Isolated**（未实现）：把 `[base, base+size)` 映射进组件地址空间，
///   返回 mapped VA；未映射地址访问由页表 fault 强制。上层 driver 不变。
///
/// 刻意不引入 ExecutionDomain registry — 只有真实存在第二个域时才需要。
fn resolve_mapping(base: usize, size: usize) -> DeviceMapping {
    DeviceMapping {
        mmio: base as *mut u8,
        mmio_len: size,
    }
}

/// 主动释放 device ownership。
///
/// **拆机顺序**：仍有 live IRQ route / DMA mapping 时返回
/// [`DeviceReleaseError::HasChildren`]——先静默设备、释放 IRQ/DMA，再释放 device。
/// 检查在 device 表锁内完成（device 锁是最外层，见 `resource` 模块的锁序）。
pub fn release(ctx: &RequestContext, device: DeviceId) -> Result<(), DeviceReleaseError> {
    let Some(machine) = machine::committed() else {
        return Err(DeviceReleaseError::DeviceNotFound);
    };
    if machine.devices.get(device.raw() as usize).is_none() {
        return Err(DeviceReleaseError::DeviceNotFound);
    }

    let _guard = IrqSaveGuard::new();
    let mut table = get_table().lock();
    if table.owner(device) != Some(ctx.component) {
        // 未认领 / 非 owner 都视为不可释放（不泄漏"谁拥有"给别人）。
        return Err(match table.owner(device) {
            Some(_) => DeviceReleaseError::NotOwner,
            None => DeviceReleaseError::DeviceNotFound,
        });
    }
    // 子项检查与释放同锁完成：不允许"先检查无子项、随后子项才被 grant"的窗口。
    if super::irq::has_route_for_device(device) || super::dma::has_mapping_for_device(device) {
        return Err(DeviceReleaseError::HasChildren);
    }
    table.release(ctx.component, device)?;
    emit(TraceEvent::ResourceRevoke {
        component: ctx.component,
        kind: ResourceKind::Device,
        id: u64::from(device.raw()),
    });
    Ok(())
}

/// 失败路径：撤销该 component 的全部 device 并 quarantine。
pub fn quarantine_owner(owner: ComponentId) {
    let _guard = IrqSaveGuard::new();
    let mut table = get_table().lock();
    // 真实表长（= 快照设备数）迭代；不再扫描固定 0..256。
    for index in 0..table.len() {
        let Ok(raw) = u32::try_from(index) else {
            break;
        };
        let device = DeviceId::from_raw(raw);
        if table.owner(device) == Some(owner) {
            emit(TraceEvent::ResourceRevoke {
                component: owner,
                kind: ResourceKind::Device,
                id: u64::from(device.raw()),
            });
        }
    }
    table.quarantine_owner(owner);
}

#[cfg(test)]
mod tests {
    use super::{DeviceClaimError, DeviceReleaseError, DeviceTable};
    use crate::component::ComponentId;
    use crate::machine::{CompatStr, DeviceDescriptor, DeviceId, IoSpace};
    use alloc::vec;

    fn owner(raw: u32) -> ComponentId {
        ComponentId::from_raw(raw)
    }

    #[test]
    fn claim_is_exclusive_and_release_frees_for_reuse() {
        let a = owner(1);
        let b = owner(2);
        let mut table = DeviceTable::new(16);

        assert_eq!(table.claim(a, DeviceId::from_raw(7)), Ok(()));
        assert_eq!(table.owner(DeviceId::from_raw(7)), Some(a));
        // 同一 owner / 其他 owner 再认领都 Busy。
        assert_eq!(
            table.claim(a, DeviceId::from_raw(7)),
            Err(DeviceClaimError::DeviceBusy)
        );
        assert_eq!(
            table.claim(b, DeviceId::from_raw(7)),
            Err(DeviceClaimError::DeviceBusy)
        );

        // 非 owner 不能释放。
        assert_eq!(
            table.release(b, DeviceId::from_raw(7)),
            Err(DeviceReleaseError::NotOwner)
        );
        // owner 释放后可被他人复用。
        assert_eq!(table.release(a, DeviceId::from_raw(7)), Ok(()));
        assert_eq!(table.owner(DeviceId::from_raw(7)), None);
        assert_eq!(table.claim(b, DeviceId::from_raw(7)), Ok(()));
    }

    /// 全宽设备身份：`DeviceId ≥ 256` 的合成设备可认领 / 释放；越界 id 返回
    /// `DeviceNotFound`（不 panic）——旧的 u8 收窄已删除。
    #[test]
    fn claim_and_release_cover_device_ids_beyond_a_byte() {
        let a = owner(1);
        let mut table = DeviceTable::new(300);
        let far = DeviceId::from_raw(260);

        assert_eq!(table.claim(a, far), Ok(()));
        assert_eq!(table.owner(far), Some(a));
        assert!(!table.is_quarantined(far));
        assert_eq!(table.release(a, far), Ok(()));
        assert_eq!(table.owner(far), None);

        // 表长之外：不存在（不 panic）。
        assert_eq!(
            table.claim(a, DeviceId::from_raw(300)),
            Err(DeviceClaimError::DeviceNotFound)
        );
        assert_eq!(
            table.release(a, DeviceId::from_raw(300)),
            Err(DeviceReleaseError::DeviceNotFound)
        );
    }

    #[test]
    fn quarantine_clears_owner_and_blocks_reclaim() {
        let a = owner(1);
        let b = owner(2);
        let mut table = DeviceTable::new(16);
        assert_eq!(table.claim(a, DeviceId::from_raw(9)), Ok(()));

        table.quarantine_owner(a);

        assert_eq!(
            table.owner(DeviceId::from_raw(9)),
            None,
            "失败后 owner 被清空"
        );
        assert!(
            table.is_quarantined(DeviceId::from_raw(9)),
            "失败设备被 quarantine"
        );
        // 任何 owner 的认领都被挡住；别的设备不受影响。
        assert_eq!(
            table.claim(a, DeviceId::from_raw(9)),
            Err(DeviceClaimError::DeviceBusy)
        );
        assert_eq!(
            table.claim(b, DeviceId::from_raw(9)),
            Err(DeviceClaimError::DeviceBusy)
        );
        assert_eq!(table.claim(b, DeviceId::from_raw(10)), Ok(()));
    }

    #[test]
    fn release_unknown_device_is_not_found() {
        let mut table = DeviceTable::new(16);
        assert_eq!(
            table.release(owner(1), DeviceId::from_raw(3)),
            Err(DeviceReleaseError::DeviceNotFound)
        );
    }

    /// 全局认领路径：合成设备表 300 项（含 `DeviceId ≥ 256` 的设备），验证
    /// resolve → 独占 → 本域窗口；越界 / PIO 的拒绝路径不变。
    #[test]
    fn claim_resolves_mmio_and_rejects_out_of_range_and_pio() {
        let _guard = crate::machine::test_support::GUARD.lock();

        let mut devices = vec![DeviceDescriptor::empty(); 300];
        let mut mmio = DeviceDescriptor::empty();
        mmio.space = IoSpace::Mmio {
            base: 0x1000_8000,
            size: 0x1000,
        };
        mmio.compatibles[0] = CompatStr::from_bytes(b"virtio,mmio");
        mmio.compat_count = 1;
        devices[0] = mmio;
        let mut pio = DeviceDescriptor::empty();
        pio.space = IoSpace::Pio {
            base: 0x3f8,
            size: 8,
        };
        devices[1] = pio;
        let mut far = DeviceDescriptor::empty();
        far.space = IoSpace::Mmio {
            base: 0x2000_0000,
            size: 0x2000,
        };
        devices[260] = far;
        let info = crate::machine::test_support::snapshot(
            crate::machine::HardwareCpuId::from_raw(0),
            10_000_000,
            vec![crate::machine::CpuInfo {
                boot_cpu: true,
                hardware_id: crate::machine::HardwareCpuId::from_raw(0),
            }],
            vec![crate::machine::MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }],
            devices,
        );
        crate::machine::test_support::install(info);
        // 测试表按 fixture 尺寸重建（全局 `Once` 表不能按用例重定容）。
        super::install_for_test(300);

        let ctx = crate::resource::RequestContext {
            component: owner(60),
            task: None,
        };
        // 越界 → DeviceNotFound。
        assert_eq!(
            super::claim(&ctx, DeviceId::from_raw(300)),
            Err(DeviceClaimError::DeviceNotFound)
        );
        // PIO → NotMmio。
        assert_eq!(
            super::claim(&ctx, DeviceId::from_raw(1)),
            Err(DeviceClaimError::NotMmio)
        );
        // MMIO → 直接拿到寄存器基址（KernelNative identity）。
        let mapping = super::claim(&ctx, DeviceId::from_raw(0)).unwrap();
        assert_eq!(mapping.mmio as usize, 0x1000_8000);
        assert_eq!(mapping.mmio_len, 0x1000);
        // 全宽：ID ≥ 256 的设备同样可认领。
        let far_mapping = super::claim(&ctx, DeviceId::from_raw(260)).expect("full-width id");
        assert_eq!(far_mapping.mmio as usize, 0x2000_0000);
        assert_eq!(far_mapping.mmio_len, 0x2000);

        super::get_table().lock().quarantine_owner(owner(60));
        super::get_table().lock().clear_quarantine();
    }
}
