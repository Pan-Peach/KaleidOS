//! MMIO authority 表 + 精确设备认领 / 单次访问（C6 起步）。
//!
//! # 模型：identity → root authority → derived authority
//!
//! ```text
//! DeviceId（身份，可复制，无权限）
//!   │  claim_device：Core 解析到设备记录 + 独占检查 + authorize
//!   ▼
//! MmioHandle（独占 root authority，锚在 device_index 上）
//!   │  claim_derived（IRQ）/ dma::alloc（DMA）从同一 handle 推导
//!   ▼
//! IrqHandle / DmaHandle（derived authority）
//! ```
//!
//! # 主线：discover → claim → access → release
//!
//! 1. **发现**（[`crate::machine::nth_compatible`]，不含 authority）：组件按
//!    compatible 纯枚举得到 `DeviceId`；枚举不分配、不触碰设备、包含已认领设备。
//! 2. [`claim_device`]：用 `DeviceId` 认领**那台确切设备**（不是"第一台匹配"）。
//!    Core 解析到设备记录、检查未被认领、过 authorize seam（phase 1 恒 allow），
//!    再 grant 出 `MmioHandle`。独占锚在**设备**（`device_index`）上：同一设备的
//!    IRQ / DMA 只能从同一个 root handle 派生——不存在"MMIO 给 A、IRQ 给 B"的拆分。
//! 3. [`read_u32`] / [`write_u32`]：每次访问都把 handle 交回 Core；Core 先验证
//!    slot/generation/owner/生命周期，再做 bounds/对齐检查，最后才碰硬件。
//! 4. [`release`]：主动交回 root authority。**root 生命周期**：只要同一
//!    `device_index` 上还有 live 的 IRQ / DMA 子 authority，就拒绝（`-EBUSY`）——
//!    优雅拆机顺序是「静默设备 → 释放 IRQ/DMA → 停 MMIO lease → 释放 MMIO root」。
//!
//! # 加锁纪律（root 派生必须与 release / failure 串行）
//!
//! **MMIO 表锁是最外层**：`mmio::release` 在持 MMIO 锁期间扫 IRQ/DMA 子表；
//! `irq::claim_derived` 在持 MMIO 锁期间 grant IRQ。`dma::alloc` 同样先在 MMIO
//! 锁下推导 `device_index`，因此「读设备身份」与「扫子 authority」互斥：要么派生
//! 先完成（release 会看到子项 → Busy），要么 release 先完成（派生看到 Stale）。
//!
//! # 实现要点
//!
//! - `claim_device`：`DeviceId` 越界 / 未提交机器信息 → `DeviceNotFound`；
//!   PIO 设备 → `NotMmio`；`device_index` 已被认领**或已被 quarantine 标记** →
//!   `DeviceBusy`。
//! - `read_u32`：`IrqSaveGuard` + 表锁 → `get(caller, handle)` → bounds
//!   `offset + 4 <= size`（checked_add）→ 对齐 `offset % 4 == 0`（非对齐
//!   volatile 读会在 S-mode fault）→ 拷出 `base` → **放锁** →
//!   `core::ptr::read_volatile`。volatile 访问绝不持表锁（trap 可能打断）。
//!
//! authorize seam：phase 1 = 恒 allow（trusted KernelNative）。当前只实现
//! allocation/ownership，不提供恶意组件隔离；未来 manifest `requires` /
//! policy / ExecutionDomain 决定 caller 是否有资格 claim。
//!
//! # 测试指引（host 可验证全路径）
//!
//! `grant` 一个 `base` 指向宿主缓冲区的 region，就能在 host 上跑完
//! validation → bounds → address math → volatile read 全路径；真实设备契约
//! 由 QEMU CoreTest 的 virtio magic（`0x74726976`）覆盖。
//!
//! # 明确砍掉（第一版勿提前长出来）
//!
//! width 1/2/4/8、activation seam、SDK、按设备授权策略、只读探测 claim
//! （generic MMIO 读可能清状态 / 弹 FIFO，Core 不学协议语义）。
//!
//! # TODO（deferred，勿在本轮实现）
//!
//! - **组件级 prober**：属于总线/协议 aware 的**组件**（不是 Core），负责兼容性
//!   排序、match table / precedence、把选中的 `DeviceId` 作为选择数据交给驱动
//!   （见 `docs/driver-model.md` §12 Q1）；Core 只提供 discovery + 精确认领。
//! - **多实例组件加载**：`ComponentRegistry` 目前按组件名唯一，一个 `virtio_blk`
//!   代码还无法对应多台设备（同 §12）。
//! - **热插拔 / 设备 reset / recovery**：失败 quarantine 保持到 reboot；不做
//!   reset/recovery 框架。
//! - **多窗口 / 非 MMIO root authority**：一个设备当前只有一个 `IoSpace`。
//! - **跨组件 handle transfer（Q2）**：`MmioHandle` 不可移交给另一个组件（组件
//!   寻址 / capability 语义未定，见 §12 Q2）。

use super::{Handle, HandleError, MmioLease, RequestContext, ResourceTable, dma, irq};
use crate::component::ComponentId;
use crate::irq::IrqSaveGuard;
use crate::machine::{self, DeviceId, IoSpace};
use spin::{Mutex, Once};

/// 一个 MMIO 资源对象：某设备的寄存器窗口。
///
/// 来自 `MachineInfo.devices`（bootstrap 发现并提交的设备真相）；
/// `device_index` 是该设备在设备表中的下标——独占认领与未来 IRQ 授予
/// 都锚在设备上，而不是锚在这段地址上。
pub struct MmioRegion {
    pub base: usize,
    pub size: usize,
    pub device_index: u8,
}

/// Core 授予组件的 MMIO authority。
pub type MmioHandle = Handle<MmioRegion>;

/// 认领失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioClaimError {
    /// `DeviceId` 不存在（越界），或机器信息尚未提交（正常组件运行期不可达）。
    DeviceNotFound,
    /// 该设备不是 MMIO（PIO 空间；本阶段不支持）。
    NotMmio,
    /// 该设备已被认领（一台设备最多一个 owner），或已被失败 quarantine 标记。
    DeviceBusy,
    /// Core 策略拒绝（phase 1 恒 allow；留给未来 manifest/policy/ExecutionDomain）。
    Denied,
}

/// 单次访问 / 释放失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioError {
    /// handle 验证失败（slot/generation/owner/生命周期）。
    Handle(HandleError),
    /// `offset + 4` 超出 region 范围。
    OutOfBounds,
    /// offset 未按 4 字节对齐（非对齐 volatile 读会在 S-mode fault）。
    Unaligned,
    /// 释放 root 时该设备仍有 live 的 IRQ / DMA 子 authority（`-EBUSY`）。
    HasChildren,
}

/// MMIO 资源真相表。
pub struct MmioTable {
    table: ResourceTable<MmioRegion>,
    /// Core 拥有的失败 quarantine 标记，按 `device_index` 索引。
    ///
    /// 设备数量上限 26（`MachineInfo.devices` 定长），但 `device_index` 是 u8，
    /// 用 256 项定长表避免任何换算。组件失败后该设备保持不可认领直到 reboot。
    quarantine: [bool; 256],
}

impl MmioTable {
    pub const fn new() -> Self {
        Self {
            table: ResourceTable::new(),
            quarantine: [false; 256],
        }
    }

    pub fn grant(&mut self, owner: ComponentId, region: MmioRegion) -> MmioHandle {
        self.table.grant(owner, region)
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得 MMIO 对象。
    pub fn get(&self, caller: ComponentId, handle: MmioHandle) -> Result<&MmioRegion, HandleError> {
        self.table.get(caller, handle)
    }

    pub fn revoke_owner(&mut self, owner: ComponentId) {
        self.table.revoke_owner(owner)
    }

    /// 该设备是否处于失败 quarantine（Core 拥有的不可用标记）。
    ///
    /// 组件失败 = 逻辑死亡、物理驻留：revoke authority ≠ 设备可被下一个驱动安全
    /// 复用（设备可能仍被硬件引用 / 未静默）。phase 1 标记后保持到 reboot。优雅、
    /// 协作式 quiesce 的 [`release`] 不进入 quarantine，设备仍可复用。
    pub fn is_quarantined(&self, device_index: u8) -> bool {
        self.quarantine[device_index as usize]
    }

    /// 撤销 owner 的全部 MMIO authority，并把其占用的每个 `device_index` 标记为
    /// quarantine（失败路径专用）。先记标记、再 revoke。
    pub fn quarantine_owner(&mut self, owner: ComponentId) {
        for slot in self.table.slots_mut() {
            if slot.owner() != owner {
                continue;
            }
            if let Some(region) = slot.object() {
                self.quarantine[region.device_index as usize] = true;
            }
            slot.revoke();
        }
    }

    pub fn release(&mut self, caller: ComponentId, handle: MmioHandle) -> Result<(), HandleError> {
        self.table.release(caller, handle)
    }

    /// 该设备（`device_index`）是否已被任意 live slot 认领。
    /// 独占锚在设备上：同一台设备最多一个 owner（同一 root 派生 IRQ / DMA）。
    pub fn holds_device(&self, device_index: u8) -> bool {
        self.table.slots().iter().any(|slot| {
            slot.object()
                .is_some_and(|region| region.device_index == device_index)
        })
    }

    /// 独占检查 + grant 的原子核心（调用方已完成 `DeviceId` → 设备记录的解析）。
    ///
    /// 检查与 grant 在同一表锁内完成，因此两个竞争调用者一个成功、一个
    /// `DeviceBusy`；quarantine 与 live claim 对普通认领同样返回 `DeviceBusy`。
    fn claim_checked(
        &mut self,
        owner: ComponentId,
        base: usize,
        size: usize,
        device_index: u8,
    ) -> Result<MmioHandle, MmioClaimError> {
        if self.is_quarantined(device_index) || self.holds_device(device_index) {
            return Err(MmioClaimError::DeviceBusy);
        }
        Ok(self.grant(
            owner,
            MmioRegion {
                base,
                size,
                device_index,
            },
        ))
    }

    /// 清空失败 quarantine（**仅测试**：进程全局表不能在用例间回退）。
    #[cfg(test)]
    pub(crate) fn clear_quarantine(&mut self) {
        self.quarantine = [false; 256];
    }
}

impl Default for MmioTable {
    fn default() -> Self {
        Self::new()
    }
}

// —— 全局（`handle::init` 初始化；测试用 `MmioTable::new()`）——

static TABLE: Once<Mutex<MmioTable>> = Once::new();

/// 初始化全局 MMIO 表（`handle::init` 调用一次）。
pub fn init() {
    TABLE.call_once(|| Mutex::new(MmioTable::new()));
}

/// 取全局 MMIO 表（init 后可用）。
pub fn get_table() -> &'static Mutex<MmioTable> {
    TABLE.get().expect("mmio table not initialized")
}

/// 认领**一台确切设备**的 MMIO root authority：resolve → authorize → grant。
///
/// `device` 由发现阶段（[`crate::machine::nth_compatible`]）给出；Core 把它解析
/// 回设备记录。`DeviceId` 本身不授予任何东西——真正的 authority 只在这里，经
/// 独占检查 + authorize seam 后 grant。
///
/// - `device` 越界 / 机器信息未提交 → `DeviceNotFound`；
/// - 设备是 PIO 空间 → `NotMmio`；
/// - `device_index` 已被认领**或已被 quarantine** → `DeviceBusy`（独占锚在设备上）。
///
/// 独占检查与 grant 在**同一把表锁**内完成：两个竞争调用者一个成功、一个 Busy。
pub fn claim_device(ctx: &RequestContext, device: DeviceId) -> Result<MmioHandle, MmioClaimError> {
    let Some(machine) = machine::committed() else {
        // 机器信息尚未提交（正常组件运行期不可达）
        return Err(MmioClaimError::DeviceNotFound);
    };
    let Some(descriptor) = machine.devices[..machine.dev_count].get(device.raw() as usize) else {
        return Err(MmioClaimError::DeviceNotFound);
    };
    // 只看 MMIO 空间（PIO 设备本阶段不认领）。
    let IoSpace::Mmio { base, size } = descriptor.space else {
        return Err(MmioClaimError::NotMmio);
    };
    // `claim_device` 只接受已提交设备表内的 ID，因此 index 必落在 u8 内。
    let device_index = u8::try_from(device.raw()).expect("device index within u8");

    let _guard = IrqSaveGuard::new();
    // authorize seam：phase 1 恒 allow（trusted KernelNative）。当前只实现
    // allocation/ownership，不提供恶意组件隔离；未来 manifest requires /
    // policy / ExecutionDomain 在这里决定 caller 是否有资格 claim。
    get_table()
        .lock()
        .claim_checked(ctx.component, base, size, device_index)
}

/// 派生 [`MmioLease`]：KernelNative 直接 MMIO 快路径。
///
/// Core 只在这里校验一次 handle（slot/generation/owner/生命周期），成功则
/// 返回携带 `region.base` 指针、`region.size` 长度与 `source = handle`
/// （provenance）的 lease。受信 KernelNative 驱动据此直接 volatile 访问，
/// 稳态不再 per-access 进 Core。**撤销是协作式的**：在 revoke/release 之前
/// 已经派生出去的裸指针不会被追回（见 `lease` 模块文档）。
pub fn derive_lease(ctx: &RequestContext, handle: MmioHandle) -> Result<MmioLease, MmioError> {
    let _guard = IrqSaveGuard::new();
    let table = get_table().lock();
    let region = table
        .get(ctx.component, handle)
        .map_err(MmioError::Handle)?;
    Ok(MmioLease::new(region.base as *mut u8, region.size, handle))
}

/// 单次 32-bit MMIO 读：每次调用重新验证 handle（slot/generation/owner/
/// 生命周期），过 bounds/对齐检查后由 Core 访问硬件。
pub fn read_u32(ctx: &RequestContext, handle: MmioHandle, offset: u32) -> Result<u32, MmioError> {
    let _guard = IrqSaveGuard::new();

    let addr = {
        let table = get_table().lock();
        let region = table
            .get(ctx.component, handle)
            .map_err(MmioError::Handle)?;
        let offset = offset as usize;
        if offset
            .checked_add(4)
            .filter(|&end| end <= region.size)
            .is_none()
        {
            return Err(MmioError::OutOfBounds);
        }
        if !offset.is_multiple_of(4) {
            return Err(MmioError::Unaligned);
        }
        region.base + offset
    };
    Ok(unsafe { core::ptr::read_volatile(addr as *const u32) })
}

/// 单次 32-bit MMIO 写：每次调用重新验证 handle（slot/generation/owner/
/// 生命周期），过 bounds/对齐检查后由 Core 访问硬件。
pub fn write_u32(
    ctx: &RequestContext,
    handle: MmioHandle,
    offset: u32,
    value: u32,
) -> Result<(), MmioError> {
    let _guard = IrqSaveGuard::new();

    let addr = {
        let table = get_table().lock();
        let region = table
            .get(ctx.component, handle)
            .map_err(MmioError::Handle)?;
        let offset = offset as usize;
        if offset
            .checked_add(4)
            .filter(|&end| end <= region.size)
            .is_none()
        {
            return Err(MmioError::OutOfBounds);
        }
        if !offset.is_multiple_of(4) {
            return Err(MmioError::Unaligned);
        }
        region.base + offset
    };
    // SAFETY: category 6 (alignment) and 10 (bounds) are established by the
    // checks above; `base` is the Core-authoritative MMIO region base.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) };
    Ok(())
}

/// 主动释放 MMIO root authority。
///
/// **root 生命周期**：只要同一 `device_index` 上还有 live 的 IRQ / DMA 子
/// authority，就返回 [`MmioError::HasChildren`]（`-EBUSY`）——必须先释放子项。
/// 扫描在 MMIO 表锁内进行（MMIO 锁是最外层，见模块文档的加锁纪律），因此与
/// IRQ/DMA 派生互斥：不允许"先检查无子项、随后子项才被 grant"的窗口。
///
/// 优雅拆机顺序：静默设备 → 释放 IRQ/DMA → 停 MMIO lease → 释放 MMIO root。
pub fn release(ctx: &RequestContext, handle: MmioHandle) -> Result<(), MmioError> {
    let _guard = IrqSaveGuard::new();
    let mut table = get_table().lock();
    let device_index = table
        .get(ctx.component, handle)
        .map_err(MmioError::Handle)?
        .device_index;
    if irq::has_line_for_device(device_index) || dma::has_region_for_device(device_index) {
        return Err(MmioError::HasChildren);
    }
    table
        .release(ctx.component, handle)
        .map_err(MmioError::Handle)
}

#[cfg(test)]
mod tests {
    use super::{MmioClaimError, MmioError, MmioRegion, MmioTable};
    use crate::component::ComponentId;
    use crate::handle::{HandleError, RequestContext};
    use crate::machine::{CompatStr, DeviceDescriptor, IoSpace};

    fn region(device_index: u8) -> MmioRegion {
        MmioRegion {
            base: 0x1000_0000 + device_index as usize * 0x1000,
            size: 0x1000,
            device_index,
        }
    }

    fn context(component: ComponentId) -> RequestContext {
        RequestContext {
            component,
            task: None,
        }
    }

    // ---- 表语义（现在就能绿）----

    #[test]
    fn grant_reuses_vacant_slot_with_new_generation() {
        let owner_a = ComponentId::from_raw(1);
        let owner_b = ComponentId::from_raw(2);
        let mut table = MmioTable::new();

        let old = table.grant(owner_a, region(0));
        table.revoke_owner(owner_a);
        let new = table.grant(owner_b, region(1));

        assert_eq!(old.slot(), new.slot());
        assert_ne!(old.generation(), new.generation());
        assert!(matches!(table.get(owner_a, old), Err(HandleError::Stale)));
        assert!(table.get(owner_b, new).is_ok());
    }

    #[test]
    fn revoke_owner_revokes_all_owned_slots() {
        let owner_a = ComponentId::from_raw(1);
        let owner_b = ComponentId::from_raw(2);
        let mut table = MmioTable::new();

        let first = table.grant(owner_a, region(0));
        let second = table.grant(owner_a, region(1));
        let other = table.grant(owner_b, region(2));

        table.revoke_owner(owner_a);

        assert!(matches!(table.get(owner_a, first), Err(HandleError::Stale)));
        assert!(matches!(
            table.get(owner_a, second),
            Err(HandleError::Stale)
        ));
        assert!(table.get(owner_b, other).is_ok());
    }

    #[test]
    fn release_only_releases_the_exact_handle() {
        let owner = ComponentId::from_raw(1);
        let other = ComponentId::from_raw(2);
        let mut table = MmioTable::new();

        let first = table.grant(owner, region(0));
        let second = table.grant(owner, region(1));

        assert_eq!(table.release(other, first), Err(HandleError::WrongOwner));
        assert!(table.get(owner, first).is_ok());

        assert_eq!(table.release(owner, second), Ok(()));
        assert!(table.get(owner, first).is_ok());
        assert!(matches!(table.get(owner, second), Err(HandleError::Stale)));
        assert_eq!(table.release(owner, second), Err(HandleError::Stale));
    }

    #[test]
    fn get_returns_granted_region_payload() {
        let owner = ComponentId::from_raw(1);
        let mut table = MmioTable::new();

        let handle = table.grant(owner, region(3));
        let got = table.get(owner, handle).unwrap();

        assert_eq!(got.device_index, 3);
        assert_eq!(got.base, 0x1000_3000);
        assert_eq!(got.size, 0x1000);
    }

    // ---- 设备发现 + 精确认领（identity → root authority）----

    /// MMIO 设备描述符（单 compatible）。
    fn mmio_device(compatible: &[u8], base: usize, irq: Option<u32>) -> DeviceDescriptor {
        let mut device = DeviceDescriptor::empty();
        device.space = IoSpace::Mmio { base, size: 0x1000 };
        device.irq = irq;
        device.compatibles[0] = CompatStr::from_bytes(compatible);
        device.compat_count = 1;
        device
    }

    /// 提交一份测试设备表。设备下标刻意选在 IRQ/DMA 表测试不用的 20+ 区间，
    /// 避免进程全局表之间通过 `device_index` 相互干扰。
    fn commit_test_machine() {
        use crate::machine::{self, CpuId, CpuInfo, MachineInfo, MemoryRegion};
        let mut devices = [DeviceDescriptor::empty(); 26];
        devices[20] = mmio_device(b"virtio,mmio", 0x1000_8000, Some(8));
        devices[21] = mmio_device(b"ns16550a", 0x1000_0000, Some(10));
        devices[22] = {
            let mut pio = DeviceDescriptor::empty();
            pio.space = IoSpace::Pio {
                base: 0x3f8,
                size: 8,
            };
            pio.compatibles[0] = CompatStr::from_bytes(b"pio,thing");
            pio.compat_count = 1;
            pio
        };
        devices[23] = mmio_device(b"virtio,mmio", 0x1000_7000, Some(9));
        machine::commit(MachineInfo {
            boot_hart: 0,
            timebase_frequency: 10_000_000,
            cpu_count: 1,
            cpu_info: [CpuInfo {
                boot_cpu: true,
                hart_id: CpuId::from_raw(0),
            }; 8],
            mem_count: 1,
            memory_regions: [MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }; 16],
            dev_count: 24,
            devices,
        });
    }

    /// 验收：claim 认领**确切的** DeviceId，不认领"第一台匹配"。
    /// 换台设备后仍可认领；PIO → NotMmio；越界 → NotFound；重复 → Busy。
    #[test]
    fn claim_device_selects_exact_device_and_is_exclusive() {
        use crate::machine::DeviceId;
        let _guard = crate::machine::test_support::GUARD.lock();
        crate::handle::init();
        commit_test_machine();

        let owner = ComponentId::from_raw(71);
        let ctx = context(owner);

        let first_id = crate::machine::nth_compatible(b"virtio,mmio", 0).unwrap();
        let second_id = crate::machine::nth_compatible(b"virtio,mmio", 1).unwrap();
        assert_eq!(first_id, DeviceId::from_raw(20));
        assert_eq!(second_id, DeviceId::from_raw(23));

        let first = super::claim_device(&ctx, first_id).unwrap();
        {
            // 表锁作用域内用完即释放：后面再 claim 会重新 lock 同一把锁。
            let table = super::get_table().lock();
            let region = table.get(owner, first).unwrap();
            assert_eq!(region.base, 0x1000_8000);
            assert_eq!(region.size, 0x1000);
            assert_eq!(region.device_index, 20);
        }

        // 同一台设备（包括同一 owner）再认领 → Busy。
        assert_eq!(
            super::claim_device(&ctx, first_id),
            Err(MmioClaimError::DeviceBusy)
        );

        // 另一台 virtio 仍可认领，拿到不同 handle。
        let second = super::claim_device(&ctx, second_id).unwrap();
        assert_ne!(first, second);

        // PIO 设备 → NotMmio。
        assert_eq!(
            super::claim_device(&ctx, DeviceId::from_raw(22)),
            Err(MmioClaimError::NotMmio)
        );
        // 不存在 / 越界 → NotFound。
        assert_eq!(
            super::claim_device(&ctx, DeviceId::from_raw(99)),
            Err(MmioClaimError::DeviceNotFound)
        );

        super::get_table().lock().revoke_owner(owner);
    }

    /// 验收：释放后旧 token stale，重新认领得到**新的** handle（新 generation）。
    #[test]
    fn claim_after_release_returns_new_handle_and_stales_old() {
        use crate::machine::DeviceId;
        let _guard = crate::machine::test_support::GUARD.lock();
        crate::handle::init();
        commit_test_machine();

        let owner = ComponentId::from_raw(72);
        let ctx = context(owner);
        let id = DeviceId::from_raw(20);

        let first = super::claim_device(&ctx, id).unwrap();
        assert_eq!(super::release(&ctx, first), Ok(()));
        assert!(matches!(
            super::get_table().lock().get(owner, first),
            Err(HandleError::Stale)
        ));

        let second = super::claim_device(&ctx, id).unwrap();
        assert_ne!(first, second);
        assert!(super::get_table().lock().get(owner, second).is_ok());
        super::get_table().lock().revoke_owner(owner);
    }

    /// 验收：枚举包含已认领设备，且顺序跨 claim/release 稳定。
    #[test]
    fn enumeration_includes_claimed_devices_and_is_stable_across_claim_release() {
        use crate::machine::DeviceId;
        let _guard = crate::machine::test_support::GUARD.lock();
        crate::handle::init();
        commit_test_machine();

        let owner = ComponentId::from_raw(73);
        let ctx = context(owner);
        let before: [DeviceId; 2] = [
            crate::machine::nth_compatible(b"virtio,mmio", 0).unwrap(),
            crate::machine::nth_compatible(b"virtio,mmio", 1).unwrap(),
        ];

        let first = super::claim_device(&ctx, before[0]).unwrap();
        // 认领之后枚举仍然列出它（顺序 / 身份不变）。
        assert_eq!(
            crate::machine::nth_compatible(b"virtio,mmio", 0),
            Ok(before[0])
        );
        assert_eq!(
            crate::machine::nth_compatible(b"virtio,mmio", 1),
            Ok(before[1])
        );

        assert_eq!(super::release(&ctx, first), Ok(()));
        // 释放之后同样不变。
        assert_eq!(
            crate::machine::nth_compatible(b"virtio,mmio", 0),
            Ok(before[0])
        );
        assert_eq!(
            crate::machine::nth_compatible(b"virtio,mmio", 1),
            Ok(before[1])
        );
    }

    /// 验收：失败 quarantine 标记按 device_index 挡住普通认领；优雅 release 不标记。
    #[test]
    fn quarantine_owner_marks_device_and_blocks_reclaim() {
        let owner = ComponentId::from_raw(80);
        let other = ComponentId::from_raw(81);
        let mut table = MmioTable::new();
        let handle = table.grant(owner, region(5));
        assert!(!table.is_quarantined(5));
        assert!(table.get(owner, handle).is_ok());

        table.quarantine_owner(owner);

        assert!(table.is_quarantined(5), "失败后设备必须被标记");
        assert!(matches!(table.get(owner, handle), Err(HandleError::Stale)));
        // 同一 owner 或其他 owner 的普通认领都被 quarantine 挡住。
        assert_eq!(
            table.claim_checked(owner, 0x1000_5000, 0x1000, 5),
            Err(MmioClaimError::DeviceBusy)
        );
        assert_eq!(
            table.claim_checked(other, 0x1000_5000, 0x1000, 5),
            Err(MmioClaimError::DeviceBusy)
        );
        // 别的设备不受影响。
        assert!(table.claim_checked(other, 0x1000_6000, 0x1000, 6).is_ok());
    }

    /// 验收：普通 release 可回收（不 quarantine），设备仍可再次认领。
    #[test]
    fn graceful_release_does_not_quarantine_device() {
        let owner = ComponentId::from_raw(82);
        let mut table = MmioTable::new();
        let handle = table.grant(owner, region(7));
        assert_eq!(table.release(owner, handle), Ok(()));
        assert!(!table.is_quarantined(7));
        assert!(table.claim_checked(owner, 0x1000_7000, 0x1000, 7).is_ok());
    }

    // ---- root 生命周期：有 live IRQ 子项时拒绝释放 ----

    /// 验收：同一 `device_index` 上仍有 live IRQ 子 authority 时，release 拒绝
    /// （`-EBUSY`）；子项释放后 root 才能释放。
    #[test]
    fn release_refuses_while_irq_child_is_live() {
        use crate::handle::irq::{self, Irq};
        let _guard = crate::machine::test_support::GUARD.lock();
        crate::handle::init();

        let owner = ComponentId::from_raw(83);
        let ctx = context(owner);
        let mmio = super::get_table().lock().grant(owner, region(18));
        let irq_handle = irq::get_table().lock().grant(owner, Irq::new(218, 18));

        assert_eq!(super::release(&ctx, mmio), Err(MmioError::HasChildren));

        // 释放子项后 root 可释放。
        assert_eq!(irq::get_table().lock().release(owner, irq_handle), Ok(()));
        assert_eq!(super::release(&ctx, mmio), Ok(()));
    }

    /// host 缓冲当 MMIO 靶子：validation → bounds → 对齐 → volatile read 全路径。
    #[test]
    fn read_u32_validates_then_reads_through_handle() {
        crate::handle::init();
        let mut buf = [0u32; 4];
        buf[0] = 0x7472_6976; // host 缓冲区模拟设备寄存器（virtio magic）
        let owner = ComponentId::from_raw(11);
        let other = ComponentId::from_raw(12);

        let handle = super::get_table().lock().grant(
            owner,
            MmioRegion {
                base: buf.as_ptr() as usize,
                size: core::mem::size_of_val(&buf),
                device_index: 0,
            },
        );

        assert_eq!(super::read_u32(&context(owner), handle, 0), Ok(0x7472_6976));
        assert_eq!(super::read_u32(&context(owner), handle, 4), Ok(0));

        // 对抗：wrong owner / 越界 / 非对齐 / revoke 后 stale
        assert_eq!(
            super::read_u32(&context(other), handle, 0),
            Err(MmioError::Handle(HandleError::WrongOwner))
        );
        assert_eq!(
            super::read_u32(&context(owner), handle, 16),
            Err(MmioError::OutOfBounds)
        );
        assert_eq!(
            super::read_u32(&context(owner), handle, 2),
            Err(MmioError::Unaligned)
        );

        super::get_table().lock().revoke_owner(owner);
        assert_eq!(
            super::read_u32(&context(owner), handle, 0),
            Err(MmioError::Handle(HandleError::Stale))
        );
    }

    #[test]
    fn write_u32_round_trips_and_rejects_invalid_access() {
        crate::handle::init();
        let mut buf = [0u32; 4];
        let owner = ComponentId::from_raw(13);
        let other = ComponentId::from_raw(14);
        let owner_ctx = RequestContext {
            component: owner,
            task: None,
        };
        let other_ctx = RequestContext {
            component: other,
            task: None,
        };
        let handle = super::get_table().lock().grant(
            owner,
            MmioRegion {
                base: buf.as_mut_ptr() as usize,
                size: core::mem::size_of_val(&buf),
                // 选一个 IRQ/DMA 测试不用的 device_index：free `release` 会扫子表。
                device_index: 19,
            },
        );

        assert_eq!(super::write_u32(&owner_ctx, handle, 0, 0x7472_6976), Ok(()));
        assert_eq!(super::read_u32(&owner_ctx, handle, 0), Ok(0x7472_6976));
        assert_eq!(
            super::write_u32(&other_ctx, handle, 0, 0),
            Err(MmioError::Handle(HandleError::WrongOwner))
        );
        assert_eq!(
            super::write_u32(&owner_ctx, handle, 16, 0),
            Err(MmioError::OutOfBounds)
        );
        assert_eq!(
            super::write_u32(&owner_ctx, handle, 2, 0),
            Err(MmioError::Unaligned)
        );

        assert_eq!(super::release(&owner_ctx, handle), Ok(()));
        assert_eq!(
            super::write_u32(&owner_ctx, handle, 0, 0),
            Err(MmioError::Handle(HandleError::Stale))
        );
        assert_eq!(
            super::release(&owner_ctx, handle),
            Err(MmioError::Handle(HandleError::Stale))
        );
    }

    #[test]
    fn write_u32_rejects_stale_handle_after_owner_revoke() {
        crate::handle::init();
        let mut buf = [0u32; 1];
        let owner = ComponentId::from_raw(15);
        let ctx = context(owner);
        let handle = super::get_table().lock().grant(
            owner,
            MmioRegion {
                base: buf.as_mut_ptr() as usize,
                size: core::mem::size_of_val(&buf),
                device_index: 0,
            },
        );

        super::get_table().lock().revoke_owner(owner);

        assert_eq!(
            super::write_u32(&ctx, handle, 0, 0),
            Err(MmioError::Handle(HandleError::Stale))
        );
    }

    // ---- MmioLease 派生（KernelNative 直接 MMIO 快路径）----

    /// Core 一次性校验后派生 lease：指针 / 长度 / source 都来自已 grant 的 region。
    #[test]
    fn derive_lease_carries_region_ptr_len_and_source() {
        crate::handle::init();
        let mut buf = [0u32; 4];
        let owner = ComponentId::from_raw(21);
        let handle = super::get_table().lock().grant(
            owner,
            MmioRegion {
                base: buf.as_mut_ptr() as usize,
                size: core::mem::size_of_val(&buf),
                device_index: 200,
            },
        );

        let lease = super::derive_lease(&context(owner), handle).unwrap();

        assert_eq!(lease.as_ptr(), buf.as_mut_ptr().cast::<u8>());
        assert_eq!(lease.len(), core::mem::size_of_val(&buf));
        assert_eq!(lease.source(), handle);
        assert!(!lease.is_empty());

        // 归还全局表，避免污染 `claim` 的设备独占检查。
        super::get_table().lock().revoke_owner(owner);
    }

    /// 对抗：wrong owner / release 后 / revoke_owner 后一律拒绝，不产出 lease。
    ///
    /// 注意：这里只验证 **尚未派生** 时不放行。至于在 `release`/`revoke_owner`
    /// **之前**已经派生出去的裸指针，Core 不会追回——KernelNative 撤销是协作
    /// 式的，没有硬件 fault 可测（见 `lease` 模块文档与 `docs/driver-model.md` §3）。
    #[test]
    fn derive_lease_rejects_invalid_handle() {
        crate::handle::init();
        let mut buf = [0u32; 4];
        let owner = ComponentId::from_raw(22);
        let other = ComponentId::from_raw(23);
        let base = buf.as_mut_ptr() as usize;
        let size = core::mem::size_of_val(&buf);
        let released = super::get_table().lock().grant(
            owner,
            MmioRegion {
                base,
                size,
                device_index: 201,
            },
        );
        let revoked = super::get_table().lock().grant(
            owner,
            MmioRegion {
                base,
                size,
                device_index: 202,
            },
        );

        // wrong owner
        assert_eq!(
            super::derive_lease(&context(other), released).unwrap_err(),
            MmioError::Handle(HandleError::WrongOwner)
        );

        // release 后 stale
        assert!(super::get_table().lock().release(owner, released).is_ok());
        assert_eq!(
            super::derive_lease(&context(owner), released).unwrap_err(),
            MmioError::Handle(HandleError::Stale)
        );

        // revoke_owner 后 stale
        super::get_table().lock().revoke_owner(owner);
        assert_eq!(
            super::derive_lease(&context(owner), revoked).unwrap_err(),
            MmioError::Handle(HandleError::Stale)
        );
    }

    /// 性能基线（`#[ignore]`，不进 `make check`）：单次 `read_u32` 全路径 vs 裸 volatile。
    ///
    /// 用法：`make bench`，或
    /// `cargo test --release -p kernel --lib -- bench -- --ignored --nocapture`。
    ///
    /// 读法：看**同一台机器上的相对变化**——改 read_u32 / 表锁 / 验证路径后重跑对比。
    /// 绝对值不代表目标机：host 的 irq-guard 是 no-op（真机多 ~2 条 CSR 指令），
    /// host 原子操作比 RV64 本地原子贵；而真实 MMIO 访问本身的成本远在此之上
    /// （QEMU 设备模型 ~µs 级，真机总线往返 ~几十~几百 ns）。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_read_u32_baseline() {
        crate::handle::init();
        let mut buf = [0u32; 4];
        buf[0] = 0x7472_6976;
        let owner = ComponentId::from_raw(99);
        let handle = super::get_table().lock().grant(
            owner,
            MmioRegion {
                base: buf.as_ptr() as usize,
                size: core::mem::size_of_val(&buf),
                device_index: 0,
            },
        );

        const N: u32 = 1_000_000;

        // 热身：避免首次缓存/分支预测冷启动污染数字
        let mut warm = 0u32;
        for _ in 0..10_000 {
            warm = warm.wrapping_add(super::read_u32(&context(owner), handle, 0).unwrap());
        }

        let t0 = std::time::Instant::now();
        let mut acc = 0u32;
        for _ in 0..N {
            acc = acc.wrapping_add(super::read_u32(&context(owner), handle, 0).unwrap());
        }
        let mediated = t0.elapsed();

        let t1 = std::time::Instant::now();
        let mut raw = 0u32;
        for _ in 0..N {
            raw = raw.wrapping_add(unsafe { core::ptr::read_volatile(buf.as_ptr()) });
        }
        let direct = t1.elapsed();

        std::println!(
            "[bench] read_u32 mediated: {:.1} ns/call | direct volatile: {:.2} ns/call | warm={warm:#x} acc={acc:#x} raw={raw:#x}",
            mediated.as_nanos() as f64 / N as f64,
            direct.as_nanos() as f64 / N as f64,
        );
    }
}
