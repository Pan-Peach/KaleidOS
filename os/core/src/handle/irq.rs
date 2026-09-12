//! IRQ authority 表 + 外部中断投递（C6 骨架）。
//!
//! # 主线：request → authorize → grant → register → enable → dispatch
//!
//! 1. [`claim`]：组件用 compatible 字符串认领**设备的 IRQ 线**（不是认领一个
//!    裸中断号）。查 `MachineInfo.devices` 找匹配、取设备的 `irq`（PLIC global
//!    interrupt id）、过 authorize seam（phase 1 恒 allow），授予 `IrqHandle`。
//!    独占锚在**中断号**上：一条线最多一个 owner（重复认领 = duplicate claim）。
//! 2. [`IrqTable::set_delivery`]：owner 注册处理函数（`extern "C" fn(ctx)` +
//!    opaque ctx）。delivery 存在 slot 里，**随 revoke/release 一起消失**——
//!    组件失败/卸载后不会再有回调进它的代码。
//! 3. [`enable`]：Core 验证 handle + delivery 后，才去配置中断控制器（PLIC enable）。
//! 4. 投递：trap 的外部中断分支 → `crate::irq::on_external` → `crate::irq::route`
//!    查表找到该线的 owner delivery：
//!    - `Callback`：**锁外**调用组件 handler（受信 KernelNative）；
//!    - `Polled`：顶半部只计数并掩蔽该线（[`IrqTable::note_polled_event`]），
//!      驱动任务随后 [`IrqTable::poll`] 读计数、处理设备、[`IrqTable::ack`]。
//! 5. [`IrqTable::set_polled`]：owner 把该线切成轮询投递（不装回调）。回调路径
//!    与 Polled 路径互斥（`delivery` 是 `Option<IrqDelivery>`）。
//!
//! # 实现要点
//!
//! - `claim`：遍历 `MachineInfo.devices`，找第一台「compatible 匹配、有 `irq`、
//!   且该中断号未被认领」的设备。同名设备多台（QEMU 有 8 个 virtio-mmio）时，
//!   被占的跳过；有匹配但都不可用才 `LineBusy` / `DeviceHasNoIrq`；一台都没有
//!   才 `DeviceNotFound`。
//! - `device_index` 与 MMIO 表锚在同一台设备上：同一设备的 MMIO 与 IRQ 应归同一
//!   owner（见 `handle/mod.rs` 的「IRQ 从同一 owner 派生」）。**跨表校验**（claim
//!   要求 caller 已持有该设备的 `MmioHandle`）是明确的 TODO seam，第一版不实现。
//! - 投递必须在**锁外**调用组件 handler：trap 可能重入，spin 锁不可重入。
//!
//! # 测试指引（host 可验证）
//!
//! 表语义 + delivery 注册 + `claim`/`route` 都能 host 测（`claim` 用例提交一份
//! `MachineInfo`，与 MMIO 的 claim 用例用 `machine::test_support::GUARD` 互斥）。
//! 真实的控制器契约（PLIC 寄存器真的被写、外部中断真的到达）由 QEMU CoreTest
//! 与 ArchTest 覆盖。
//!
//! # 明确砍掉（第一版勿提前长出来）
//!
//! 独立 mask ABI 出口（Polled 掩蔽完全在 Core 内部完成）、release ABI 出口、
//! 优先级/触发方式配置、共享中断线、per-line 统计、SMP affinity、把 PLIC 从
//! arch 降级为 Driver Component。

use super::{Handle, HandleError, ResourceTable};
use crate::component::ComponentId;
use crate::irq::IrqSaveGuard;
use crate::machine;
use arch::{InterruptController, InterruptImpl};
use spin::{Mutex, Once};

/// 一条 IRQ 资源对象：某设备的中断线。
///
/// `number` = 设备的 PLIC global interrupt id（来自 `MachineInfo.devices[..].irq`）；
/// `device_index` = 设备在设备表中的下标（与 MMIO 表锚在同一台设备上）。
pub struct Irq {
    pub number: u32,
    pub device_index: u8,
    /// 投递目标：组件注册的处理函数 + opaque context（`None` = 尚未注册）。
    pub delivery: Option<IrqDelivery>,
    /// `Polled` 线累计的事件数（Core 真相，顶半部锁内自增）。
    pub events: u64,
    /// `Polled` 线是否已被 Core 掩蔽：首个事件置位，`ack` 清除。
    pub masked: bool,
}

impl Irq {
    /// 新建一条尚未注册投递的 IRQ 资源对象。
    pub const fn new(number: u32, device_index: u8) -> Self {
        Self {
            number,
            device_index,
            delivery: None,
            events: 0,
            masked: false,
        }
    }
}

/// Core 授予组件的 IRQ authority。
pub type IrqHandle = Handle<Irq>;

/// 组件提供的中断处理函数（phase 1 KernelNative：direct call）。
///
/// `ctx` 原样回传给组件，Core 不解引用——与 interface registry 的 versioned
/// vtable `ctx` 同一条生命周期契约（provider Ready 期间有效）。
pub type IrqHandler = extern "C" fn(ctx: *mut ());

/// 一条 IRQ 的投递目标。
///
/// - `Callback`：trap 上下文直接调用组件处理函数（仅受信 KernelNative）；
///   函数地址与 context 以 `usize` 保存（同 arch 的 `TIMER_HANDLER: AtomicUsize`），
///   这样全局表不必把裸指针放进 `Send` 容器。
/// - `Polled`：不装回调——顶半部只计数并掩蔽该线，owner 任务轮询 `poll` 后
///   `ack`（见模块文档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqDelivery {
    /// 受信 KernelNative 回调：`handler(ctx)` 在 trap 上下文锁外调用。
    Callback { handler: usize, ctx: usize },
    /// 轮询投递：Core 计数 + 掩蔽，组件任务 `poll`/`ack`。
    Polled,
}

impl IrqDelivery {
    /// 从 ABI 收到的函数指针 + context 构造 `Callback` 投递。
    pub fn new(handler: IrqHandler, ctx: *mut ()) -> Self {
        Self::Callback {
            handler: handler as usize,
            ctx: ctx as usize,
        }
    }
}

/// 认领失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqClaimError {
    /// 已发现设备中没有 compatible 匹配项（机器信息尚未提交时同样返回此值；
    /// 正常组件运行期不可达）。
    DeviceNotFound,
    /// 匹配设备没有中断线（FDT 无 `interrupts` / `irq == None`）。
    DeviceHasNoIrq,
    /// 匹配设备的中断线已被认领（一条线最多一个 owner；duplicate claim）。
    LineBusy,
    /// Core 策略拒绝（phase 1 恒 allow；留给未来 manifest/policy/ExecutionDomain）。
    Denied,
}

/// 投递 / 使能失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqError {
    /// handle 验证失败（slot/generation/owner/生命周期）。
    Handle(HandleError),
    /// 尚未注册处理函数就试图使能该线。
    NoDelivery,
    /// 该线不是 `Polled` 投递（对 `Callback` / 未注册的线做 `poll`/`ack`）。
    NotPolled,
}

/// IRQ 资源真相表。
pub struct IrqTable {
    table: ResourceTable<Irq>,
}

impl IrqTable {
    pub const fn new() -> Self {
        Self {
            table: ResourceTable::new(),
        }
    }

    pub fn grant(&mut self, owner: ComponentId, irq: Irq) -> IrqHandle {
        self.table.grant(owner, irq)
    }

    /// 验证 slot、generation、owner 和资源生命周期后取得 IRQ 对象。
    pub fn get(&self, caller: ComponentId, handle: IrqHandle) -> Result<&Irq, HandleError> {
        self.table.get(caller, handle)
    }

    /// 注册 / 替换该线的投递目标（Core 验证 handle 成立后才写入）。
    pub fn set_delivery(
        &mut self,
        caller: ComponentId,
        handle: IrqHandle,
        delivery: IrqDelivery,
    ) -> Result<(), IrqError> {
        let irq = self
            .table
            .get_mut(caller, handle)
            .map_err(IrqError::Handle)?;
        irq.delivery = Some(delivery);
        Ok(())
    }

    /// 把该线切成轮询投递（`delivery = Some(Polled)`，不装回调）。
    /// Core 验证 handle 成立后才写入。
    pub fn set_polled(&mut self, caller: ComponentId, handle: IrqHandle) -> Result<(), IrqError> {
        let irq = self
            .table
            .get_mut(caller, handle)
            .map_err(IrqError::Handle)?;
        irq.delivery = Some(IrqDelivery::Polled);
        Ok(())
    }

    /// 读取 `Polled` 线累计的事件数（不清零——驱动按 delta 判断新事件）。
    /// 非 `Polled` 线 → [`IrqError::NotPolled`]。
    pub fn poll(&self, caller: ComponentId, handle: IrqHandle) -> Result<u64, IrqError> {
        let irq = self.table.get(caller, handle).map_err(IrqError::Handle)?;
        match irq.delivery {
            Some(IrqDelivery::Polled) => Ok(irq.events),
            _ => Err(IrqError::NotPolled),
        }
    }

    /// 确认 `Polled` 事件：清除 `masked` 并返回中断号，供调用方在**锁外**
    /// `InterruptImpl::enable(number)` 重新放行该线。非 `Polled` → `NotPolled`。
    ///
    /// 返回中断号而不是在锁内 `enable`：trap 可重入、MMIO 慢，控制器写必须
    /// 放锁后做（同 `complete`）。
    pub fn ack(&mut self, caller: ComponentId, handle: IrqHandle) -> Result<u32, IrqError> {
        let irq = self
            .table
            .get_mut(caller, handle)
            .map_err(IrqError::Handle)?;
        match irq.delivery {
            Some(IrqDelivery::Polled) => {
                irq.masked = false;
                Ok(irq.number)
            }
            _ => Err(IrqError::NotPolled),
        }
    }

    /// 顶半部事件计数（Core 内部，调用方持表锁）：仅对 `Polled` 线生效。
    ///
    /// `events += 1`；若尚未 `masked`，置 `masked = true` 并返回 `Some(number)`
    /// ——调用方据此在**锁外** `InterruptImpl::disable(number)`，把电平触发源
    /// 在协作调度下提前掩蔽（防止风暴）。已掩蔽则只计数、返回 `None`。
    /// 非 `Polled` / 未知线 → `None`。
    pub fn note_polled_event(&mut self, number: u32) -> Option<u32> {
        for slot in self.table.slots_mut() {
            if !slot.object().is_some_and(|irq| irq.number == number) {
                continue;
            }
            // 上面已确认 object 存在。
            let irq = slot.object_mut().expect("occupied slot checked above");
            return match irq.delivery {
                Some(IrqDelivery::Polled) => {
                    irq.events = irq.events.wrapping_add(1);
                    if irq.masked {
                        None
                    } else {
                        irq.masked = true;
                        Some(number)
                    }
                }
                _ => None,
            };
        }
        None
    }

    /// 撤销 owner 全部 IRQ authority。
    ///
    /// 若被撤销的是已 `masked` 的 `Polled` 线，**保持控制器上的 disable 状态**
    /// （不做任何 arch `enable`）——该线已无 owner，重新放行只会投递到无人认领
    /// 的线；下一次合法 claim/`enable` 会重新配置它。
    pub fn revoke_owner(&mut self, owner: ComponentId) {
        self.table.revoke_owner(owner)
    }

    /// 组件主动释放一条 IRQ authority。
    ///
    /// 同 `revoke_owner`：已 `masked` 的 `Polled` 线释放后**保持 disabled**
    /// （Core 不代 owner 回调 arch `enable`）。
    pub fn release(&mut self, caller: ComponentId, handle: IrqHandle) -> Result<(), HandleError> {
        self.table.release(caller, handle)
    }

    /// 该中断号是否已被任意 live slot 认领。
    /// 独占锚在中断号上：一条线最多一个 owner（重复认领 = duplicate claim）。
    pub fn holds_line(&self, number: u32) -> bool {
        self.table
            .slots()
            .iter()
            .any(|slot| slot.object().is_some_and(|irq| irq.number == number))
    }

    /// 取该中断号已注册的投递目标（`IrqDelivery` 是 Copy）。
    /// 供中断上下文的 `route` **在锁内取一份拷贝、放锁后再调用**。
    pub fn delivery_of(&self, number: u32) -> Option<IrqDelivery> {
        self.table.slots().iter().find_map(|slot| {
            slot.object()
                .filter(|irq| irq.number == number)
                .and_then(|irq| irq.delivery)
        })
    }
}

impl Default for IrqTable {
    fn default() -> Self {
        Self::new()
    }
}

// —— 全局（`handle::init` 初始化；测试用 `IrqTable::new()`）——

static TABLE: Once<Mutex<IrqTable>> = Once::new();

/// 初始化全局 IRQ 表（`handle::init` 调用一次）。
pub fn init() {
    TABLE.call_once(|| Mutex::new(IrqTable::new()));
}

/// 取全局 IRQ 表（init 后可用）。
pub fn get_table() -> &'static Mutex<IrqTable> {
    TABLE.get().expect("irq table not initialized")
}

/// 认领设备的中断线：request → authorize → grant。
///
/// 找第一台「compatible 匹配、有 `irq`、且该中断号未被任意 owner 认领」的设备；
/// 同名设备多台时按设备表顺序取用：被占的跳过，只有当**所有**匹配设备的线都不可用
/// 时才报 `LineBusy` / `DeviceHasNoIrq`。
pub fn claim(caller: ComponentId, compatible: &[u8]) -> Result<IrqHandle, IrqClaimError> {
    let Some(machine) = machine::committed() else {
        // 机器信息尚未提交（正常组件运行期不可达）
        return Err(IrqClaimError::DeviceNotFound);
    };
    let _guard = IrqSaveGuard::new();
    let mut table = get_table().lock();
    let mut saw_match = false;
    let mut saw_line = false;
    for (index, device) in machine.devices[..machine.dev_count].iter().enumerate() {
        // compatible 匹配（任一命中即可）
        if !device.compatibles[..device.compat_count as usize]
            .iter()
            .any(|c| c.as_str().as_bytes() == compatible)
        {
            continue;
        }
        saw_match = true;
        // 设备没有中断线（FDT 无 interrupts）→ 试下一台
        let Some(number) = device.irq else {
            continue;
        };
        saw_line = true;
        // 这条线已被认领 → 试下一台同名设备（QEMU 上 8 台 virtio 各占一条线）
        if table.holds_line(number) {
            continue;
        }
        // authorize seam：phase 1 恒 allow（trusted KernelNative）。当前只实现
        // allocation/ownership，不提供恶意组件隔离；未来 manifest requires /
        // policy / ExecutionDomain 在这里决定 caller 是否有资格 claim。
        //
        // TODO(C6 seam)：可收紧为「caller 必须已持有同一设备的 MmioHandle」
        // （handle/mod.rs 的「IRQ 从同一 owner 派生」），第一版不强制。
        return Ok(table.grant(caller, Irq::new(number, index as u8)));
    }
    // 有匹配但都没线 → NoIrq；有线但全被占 → Busy；压根没有匹配 → NotFound
    Err(if !saw_match {
        IrqClaimError::DeviceNotFound
    } else if !saw_line {
        IrqClaimError::DeviceHasNoIrq
    } else {
        IrqClaimError::LineBusy
    })
}

/// 打开一条 IRQ 线：Core 先验证 handle + 已注册 delivery，才去配置中断控制器。
///
/// 表锁只覆盖验证；PLIC 寄存器与 CPU 使能位在**锁外**写（MMIO 慢，且 trap 可重入）。
pub fn enable(caller: ComponentId, handle: IrqHandle) -> Result<(), IrqError> {
    let number = {
        let table = get_table().lock();
        let irq = table.get(caller, handle).map_err(IrqError::Handle)?;
        if irq.delivery.is_none() {
            // 没注册 handler 就开线 = 中断到了没人接（且无法 complete）
            return Err(IrqError::NoDelivery);
        }
        irq.number
    };

    // 先开控制器上的线，再开 CPU 闸门（顺序反了容易吃到伪中断）
    InterruptImpl::enable(number);
    InterruptImpl::enable_external_interrupt();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Irq, IrqClaimError, IrqDelivery, IrqError, IrqTable};
    use crate::component::ComponentId;
    use crate::handle::HandleError;

    fn irq(number: u32, device_index: u8) -> Irq {
        Irq::new(number, device_index)
    }

    extern "C" fn dummy_handler(_ctx: *mut ()) {}

    // ---- 表语义（现在就能绿）----

    #[test]
    fn grant_reuses_vacant_slot_with_new_generation() {
        let owner_a = ComponentId::from_raw(1);
        let owner_b = ComponentId::from_raw(2);
        let mut table = IrqTable::new();

        let old = table.grant(owner_a, irq(8, 0));
        table.revoke_owner(owner_a);
        let new = table.grant(owner_b, irq(9, 1));

        assert_eq!(old.slot(), new.slot());
        assert_ne!(old.generation(), new.generation());
        assert!(matches!(table.get(owner_a, old), Err(HandleError::Stale)));
        assert!(table.get(owner_b, new).is_ok());
    }

    #[test]
    fn revoke_owner_revokes_all_owned_slots() {
        let owner_a = ComponentId::from_raw(1);
        let owner_b = ComponentId::from_raw(2);
        let mut table = IrqTable::new();

        let first = table.grant(owner_a, irq(8, 0));
        let second = table.grant(owner_a, irq(9, 1));
        let other = table.grant(owner_b, irq(10, 2));

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
        let mut table = IrqTable::new();

        let first = table.grant(owner, irq(8, 0));
        let second = table.grant(owner, irq(9, 1));

        assert_eq!(table.release(other, first), Err(HandleError::WrongOwner));
        assert!(table.get(owner, first).is_ok());

        assert_eq!(table.release(owner, second), Ok(()));
        assert!(table.get(owner, first).is_ok());
        assert!(matches!(table.get(owner, second), Err(HandleError::Stale)));
        assert_eq!(table.release(owner, second), Err(HandleError::Stale));
    }

    #[test]
    fn get_returns_granted_irq_payload() {
        let owner = ComponentId::from_raw(1);
        let mut table = IrqTable::new();

        let handle = table.grant(owner, irq(10, 3));
        let got = table.get(owner, handle).unwrap();

        assert_eq!(got.number, 10);
        assert_eq!(got.device_index, 3);
        assert!(got.delivery.is_none());

        // 独占锚在中断号上（不看 slot/owner）
        assert!(table.holds_line(10));
        assert!(!table.holds_line(11));
    }

    // ---- C6 行为规范（验收：claim 设备中断线 / delivery 注册）----

    /// 验收：claim 取「设备的中断线」并独占锚在中断号上。
    /// virtio 两台（irq=8/9）：第一次 8（devices[0]），第二次 9（devices[2]）；
    /// 两条线都被占后 → `LineBusy`；无中断线的匹配设备 → `DeviceHasNoIrq`；
    /// 无匹配设备 → `DeviceNotFound`。
    ///
    /// 注意：`machine::COMMITTED` 是进程全局，本用例与 `handle::mmio` 的 claim
    /// 用例靠 `machine::test_support::GUARD` 串行化（各自 commit 一份 MachineInfo）。
    #[test]
    fn claim_grants_device_irq_and_is_exclusive() {
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
        // 有匹配 compatible 但没有中断线
        devices[1] = DeviceDescriptor {
            space: IoSpace::Mmio {
                base: 0x1000_0000,
                size: 0x100,
            },
            irq: None,
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

        let first = super::claim(owner, b"virtio,mmio").unwrap();
        {
            let table = super::get_table().lock();
            let got = table.get(owner, first).unwrap();
            assert_eq!(got.number, 8);
            assert_eq!(got.device_index, 0);
        }

        // 第一条线已被占 → 拿第二台（devices[2] 的 9），而不是 Busy
        let second = super::claim(owner, b"virtio,mmio").unwrap();
        assert_ne!(first, second);
        {
            let table = super::get_table().lock();
            let got = table.get(owner, second).unwrap();
            assert_eq!(got.number, 9);
            assert_eq!(got.device_index, 2);
        }

        assert_eq!(
            super::claim(owner, b"virtio,mmio"),
            Err(IrqClaimError::LineBusy)
        );
        assert_eq!(
            super::claim(owner, b"ns16550a"),
            Err(IrqClaimError::DeviceHasNoIrq)
        );
        assert_eq!(
            super::claim(owner, b"nope,device"),
            Err(IrqClaimError::DeviceNotFound)
        );
    }

    /// 验收：delivery 只能绑到该 handle 的 owner，且 revoke 后随 slot 一起消失。
    #[test]
    fn set_delivery_is_scoped_to_the_owning_handle() {
        let owner = ComponentId::from_raw(1);
        let other = ComponentId::from_raw(2);
        let mut table = IrqTable::new();

        let handle = table.grant(owner, irq(8, 0));
        let delivery = IrqDelivery::new(dummy_handler, core::ptr::null_mut());

        // wrong owner 不能注册
        assert_eq!(
            table.set_delivery(other, handle, delivery),
            Err(IrqError::Handle(HandleError::WrongOwner))
        );

        assert_eq!(table.set_delivery(owner, handle, delivery), Ok(()));
        assert_eq!(table.get(owner, handle).unwrap().delivery, Some(delivery));

        // revoke 后 delivery 随 slot 一起消失（不会再有回调进组件的代码）
        table.revoke_owner(owner);
        assert!(matches!(table.get(owner, handle), Err(HandleError::Stale)));
    }

    // ---- Polled 投递（C6 增量：Core 计数 + 掩蔽，驱动 poll/ack）----

    /// 验收：首事件计数 + 置 masked 并返回 `Some(number)`；已掩蔽只计数。
    #[test]
    fn note_polled_event_counts_and_masks_once() {
        let owner = ComponentId::from_raw(1);
        let mut table = IrqTable::new();
        let handle = table.grant(owner, irq(8, 0));

        table.set_polled(owner, handle).unwrap();
        {
            let got = table.get(owner, handle).unwrap();
            assert_eq!(got.delivery, Some(IrqDelivery::Polled));
            assert_eq!(got.events, 0);
            assert!(!got.masked);
        }

        // 首个事件：计数 + 置 masked，并返回中断号让顶半部在锁外 disable
        assert_eq!(table.note_polled_event(8), Some(8));
        {
            let got = table.get(owner, handle).unwrap();
            assert_eq!(got.events, 1);
            assert!(got.masked);
        }

        // 已 masked：继续计数，但不重复要求掩蔽
        assert_eq!(table.note_polled_event(8), None);
        {
            let got = table.get(owner, handle).unwrap();
            assert_eq!(got.events, 2);
            assert!(got.masked);
        }

        // 未知线 / 非 Polled 线：不动
        assert_eq!(table.note_polled_event(99), None);
    }

    /// 验收：`poll` 读累计计数，不清零。
    #[test]
    fn poll_reads_event_count_without_resetting() {
        let owner = ComponentId::from_raw(1);
        let mut table = IrqTable::new();
        let handle = table.grant(owner, irq(8, 0));
        table.set_polled(owner, handle).unwrap();

        assert_eq!(table.poll(owner, handle), Ok(0));
        table.note_polled_event(8);
        table.note_polled_event(8);
        assert_eq!(table.poll(owner, handle), Ok(2));
        // poll 是只读：计数保留
        assert_eq!(table.poll(owner, handle), Ok(2));
    }

    /// 验收：`ack` 清 `masked`、返回中断号（供锁外 re-enable），并可再次掩蔽。
    #[test]
    fn ack_clears_mask_and_returns_reenable_number() {
        let owner = ComponentId::from_raw(1);
        let mut table = IrqTable::new();
        let handle = table.grant(owner, irq(8, 0));
        table.set_polled(owner, handle).unwrap();

        assert_eq!(table.note_polled_event(8), Some(8));
        assert!(table.get(owner, handle).unwrap().masked);

        assert_eq!(table.ack(owner, handle), Ok(8));
        {
            let got = table.get(owner, handle).unwrap();
            assert!(!got.masked);
            // 计数保留（驱动用 delta 判断）
            assert_eq!(got.events, 1);
        }

        // ack 后可再次掩蔽下一条事件
        assert_eq!(table.note_polled_event(8), Some(8));
        assert!(table.get(owner, handle).unwrap().masked);
    }

    /// 验收：`poll`/`ack` 拒绝非 Polled 线；wrong owner / stale handle 被拒。
    #[test]
    fn poll_and_ack_reject_non_polled_wrong_owner_and_stale() {
        let owner = ComponentId::from_raw(1);
        let other = ComponentId::from_raw(2);
        let mut table = IrqTable::new();

        // Callback 线：poll/ack 都拒绝
        let callback = table.grant(owner, irq(8, 0));
        table
            .set_delivery(
                owner,
                callback,
                IrqDelivery::new(dummy_handler, core::ptr::null_mut()),
            )
            .unwrap();
        assert_eq!(table.poll(owner, callback), Err(IrqError::NotPolled));
        assert_eq!(table.ack(owner, callback), Err(IrqError::NotPolled));

        // 未注册 delivery 的线也拒绝
        let bare = table.grant(owner, irq(9, 1));
        assert_eq!(table.poll(owner, bare), Err(IrqError::NotPolled));
        assert_eq!(table.ack(owner, bare), Err(IrqError::NotPolled));

        // wrong owner：handle 校验先于 delivery 检查
        let polled = table.grant(owner, irq(10, 2));
        table.set_polled(owner, polled).unwrap();
        assert_eq!(
            table.poll(other, polled),
            Err(IrqError::Handle(HandleError::WrongOwner))
        );
        assert_eq!(
            table.ack(other, polled),
            Err(IrqError::Handle(HandleError::WrongOwner))
        );

        // stale：revoke 后
        table.revoke_owner(owner);
        assert_eq!(
            table.poll(owner, polled),
            Err(IrqError::Handle(HandleError::Stale))
        );
        assert_eq!(
            table.ack(owner, polled),
            Err(IrqError::Handle(HandleError::Stale))
        );
    }
}
