//! MMIO authority 表 + 设备认领 / 单次访问（C6 起步）。
//!
//! # 主线：request → authorize → grant → access
//!
//! 1. [`claim`]：组件用 compatible 字符串认领**设备**（不是认领一段地址）。
//!    查 `MachineInfo.devices` 找匹配、检查设备未被其他 owner 认领、过
//!    authorize seam（phase 1 恒 allow），然后 grant 出 `MmioHandle`。
//!    独占锚在**设备**（`device_index`）上：将来同一设备的 IRQ 也只能授予
//!    同一个 owner——不存在"MMIO 给 A、IRQ 给 B"的拆分。
//! 2. [`read_u32`] / [`write_u32`]：组件每次访问都把 handle 交回 Core；Core 先验证
//!    slot/generation/owner/生命周期，再做 bounds/对齐检查，最后才碰硬件。
//!    组件永远不持有地址（raw handle 只是 slot+generation 编码）。
//! 3. [`release`]：组件主动交回 authority（validate 后由资源表回收）。
//!
//! # 实现要点
//!
//! - `claim`：遍历 `MachineInfo.devices`，找第一台「compatible 匹配且未被
//!   认领」的 MMIO 设备：匹配 → `holds_device` 独占检查 → `grant`。
//!   同名设备可能有多台（如 QEMU 有 8 个 virtio-mmio transport）：被占的跳过，
//!   **全部**匹配设备都被认领才 `DeviceBusy`；一台都没有才 `DeviceNotFound`。
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
//! width 1/2/4/8、DMA/IRQ 派生、activation seam、SDK、按设备授权策略。

use super::{Handle, HandleError, MmioLease, RequestContext, ResourceTable};
use crate::component::ComponentId;
use crate::irq::IrqSaveGuard;
use crate::machine::{self, IoSpace};
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
    /// 已发现设备中没有 compatible 匹配项（机器信息尚未提交时同样返回此值；
    /// 正常组件运行期不可达）。
    DeviceNotFound,
    /// 匹配的设备全部已被组件认领（一台设备最多一个 owner）。
    DeviceBusy,
    /// Core 策略拒绝（phase 1 恒 allow；留给未来 manifest/policy/ExecutionDomain）。
    Denied,
}

/// 单次访问失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioError {
    /// handle 验证失败（slot/generation/owner/生命周期）。
    Handle(HandleError),
    /// `offset + 4` 超出 region 范围。
    OutOfBounds,
    /// offset 未按 4 字节对齐（非对齐 volatile 读会在 S-mode fault）。
    Unaligned,
}

/// MMIO 资源真相表。
pub struct MmioTable {
    table: ResourceTable<MmioRegion>,
}

impl MmioTable {
    pub const fn new() -> Self {
        Self {
            table: ResourceTable::new(),
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

    pub fn release(&mut self, caller: ComponentId, handle: MmioHandle) -> Result<(), HandleError> {
        self.table.release(caller, handle)
    }

    /// 该设备（`device_index`）是否已被任意 live slot 认领。
    /// 独占锚在设备上：同一台设备最多一个 owner（将来 IRQ 也从同一 owner 派生）。
    pub fn holds_device(&self, device_index: u8) -> bool {
        self.table.slots().iter().any(|slot| {
            slot.object()
                .is_some_and(|region| region.device_index == device_index)
        })
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

/// 认领设备：request → authorize → grant。
///
/// 找第一台「compatible 匹配且未被任意 owner 认领」的 MMIO 设备；
/// 同名设备有多台时按设备表顺序取用，全部被认领才 `DeviceBusy`。
pub fn claim(ctx: &RequestContext, compatible: &[u8]) -> Result<MmioHandle, MmioClaimError> {
    let Some(machine) = machine::committed() else {
        // 机器信息尚未提交（正常组件运行期不可达）
        return Err(MmioClaimError::DeviceNotFound);
    };
    let _guard = IrqSaveGuard::new();
    let mut table = get_table().lock();
    let mut saw_match = false;
    for (index, device) in machine.devices[..machine.dev_count].iter().enumerate() {
        // compatible 匹配（任一命中即可）
        if !device.compatibles[..device.compat_count as usize]
            .iter()
            .any(|c| c.as_str().as_bytes() == compatible)
        {
            continue;
        }
        // 只看 MMIO 空间（PIO 设备本阶段不认领）
        let IoSpace::Mmio { base, size } = device.space else {
            continue;
        };
        saw_match = true;
        // 已被认领 → 试下一台同名设备（QEMU 上 virtio 有 8 台 transport）
        if table.holds_device(index as u8) {
            continue;
        }
        // authorize seam：phase 1 恒 allow（trusted KernelNative）。当前只实现
        // allocation/ownership，不提供恶意组件隔离；未来 manifest requires /
        // policy / ExecutionDomain 在这里决定 caller 是否有资格 claim。
        //
        // 授出：所有权记在表上，返回凭证（handle 只含 slot/generation，不含地址）
        return Ok(table.grant(
            ctx.component,
            MmioRegion {
                base,
                size,
                device_index: index as u8,
            },
        ));
    }
    // 有匹配但全被认领 → Busy；压根没有匹配设备 → NotFound
    Err(if saw_match {
        MmioClaimError::DeviceBusy
    } else {
        MmioClaimError::DeviceNotFound
    })
}

/// 从已持有的 `MmioHandle` 推导设备身份（`device_index`）。
///
/// 供 **DMA 授权**用：DMA 不接受组件自报的设备号，必须先在 Core 里持有该设备的
/// MMIO authority。Core 校验 slot/generation/owner/生命周期后返回 region 记录的
/// `device_index`；校验失败返回 [`MmioError::Handle`]。
pub(crate) fn device_index_for(ctx: &RequestContext, handle: MmioHandle) -> Result<u8, MmioError> {
    let _guard = IrqSaveGuard::new();
    let table = get_table().lock();
    let region = table
        .get(ctx.component, handle)
        .map_err(MmioError::Handle)?;
    Ok(region.device_index)
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

/// 主动释放 MMIO authority；资源表负责 slot/generation/owner/lifecycle 验证。
pub fn release(ctx: &RequestContext, handle: MmioHandle) -> Result<(), MmioError> {
    let _guard = IrqSaveGuard::new();
    get_table()
        .lock()
        .release(ctx.component, handle)
        .map_err(MmioError::Handle)
}

#[cfg(test)]
mod tests {
    use super::{MmioClaimError, MmioError, MmioRegion, MmioTable};
    use crate::component::ComponentId;
    use crate::handle::{HandleError, RequestContext};

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

    // ---- C6 行为规范（验收：claim 设备认领 / read_u32 访问）----

    /// 验收：claim 取「第一台未认领的匹配设备」。
    /// 两台同名 virtio：第一次拿 devices[0]，第二次必须拿 devices[2]（而不是
    /// Busy）；两台都被认领后才 Busy；没有匹配设备才 NotFound。
    #[test]
    fn claim_grants_first_unclaimed_match_and_is_exclusive() {
        let _guard = crate::machine::test_support::GUARD.lock();
        use crate::machine::{
            self, CompatStr, CpuId, CpuInfo, DeviceDescriptor, IoSpace, MachineInfo, MemoryRegion,
        };

        super::init();
        let mut devices = [DeviceDescriptor::empty(); 26];
        devices[0] = DeviceDescriptor {
            space: IoSpace::Mmio {
                base: 0x1000_8000,
                size: 0x1000,
            },
            irq: Some(8),
            compatibles: [
                CompatStr::from_bytes(b"virtio,mmio"),
                CompatStr::empty(),
                CompatStr::empty(),
                CompatStr::empty(),
            ],
            compat_count: 1,
        };
        devices[1] = DeviceDescriptor {
            space: IoSpace::Mmio {
                base: 0x1000_0000,
                size: 0x100,
            },
            irq: Some(10),
            compatibles: [
                CompatStr::from_bytes(b"ns16550a"),
                CompatStr::empty(),
                CompatStr::empty(),
                CompatStr::empty(),
            ],
            compat_count: 1,
        };
        devices[2] = DeviceDescriptor {
            space: IoSpace::Mmio {
                base: 0x1000_7000,
                size: 0x1000,
            },
            irq: Some(9),
            compatibles: [
                CompatStr::from_bytes(b"virtio,mmio"),
                CompatStr::empty(),
                CompatStr::empty(),
                CompatStr::empty(),
            ],
            compat_count: 1,
        };
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
            dev_count: 3,
            devices,
        });

        let owner = ComponentId::from_raw(7);

        // 第一台 virtio（devices[0]）
        let first = super::claim(&context(owner), b"virtio,mmio").unwrap();
        {
            // 表锁作用域内用完即释放：后面再 claim 会重新 lock 同一把锁。
            let table = super::get_table().lock();
            let region = table.get(owner, first).unwrap();
            assert_eq!(region.base, 0x1000_8000);
            assert_eq!(region.size, 0x1000);
            assert_eq!(region.device_index, 0);
        }

        // 第二台 virtio（devices[2]）仍未被认领 → 必须拿到它，而不是 Busy
        let second = super::claim(&context(owner), b"virtio,mmio").unwrap();
        assert_ne!(first, second);
        {
            let table = super::get_table().lock();
            let region = table.get(owner, second).unwrap();
            assert_eq!(region.base, 0x1000_7000);
            assert_eq!(region.size, 0x1000);
            assert_eq!(region.device_index, 2);
        }

        // 两台都被认领 → Busy
        assert_eq!(
            super::claim(&context(owner), b"virtio,mmio"),
            Err(MmioClaimError::DeviceBusy)
        );
        assert_eq!(
            super::claim(&context(owner), b"nope,device"),
            Err(MmioClaimError::DeviceNotFound)
        );
    }

    /// host 缓冲当 MMIO 靶子：validation → bounds → 对齐 → volatile read 全路径。
    #[test]
    fn read_u32_validates_then_reads_through_handle() {
        super::init();
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
        super::init();
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
                device_index: 0,
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
        super::init();
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
        super::init();
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
        super::init();
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
        super::init();
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
