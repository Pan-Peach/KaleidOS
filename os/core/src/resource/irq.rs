//! IRQ routes：device → 中断线的 Core 真相（单 IRQ 模型）。
//!
//! # 主线
//!
//! ```text
//! claimed DeviceId
//!   │  kcore_irq_register(device_id, handler, ctx)
//!   │    Core: 验证 owner → 查 MachineInfo.devices[id].irq → 记 route
//!   ▼
//! kcore_irq_enable(device_id)   → 配置中断控制器（arch）
//! trap → on_irq（后端 ack/映射后）→ route(number) → 锁外调用组件 handler
//!   ▼
//! kcore_irq_disable/release(device_id)
//! ```
//!
//! 锚点是 **DeviceId**，不是 `IrqHandle`：`DeviceDescriptor` 本身带 `irq`，
//! 单 IRQ 设备下再套一层"MMIO → IRQ authority 派生"没有真实用途。
//!
//! route 表**按已提交快照的设备数定容**（boxed slice，按 `DeviceId` 索引；
//! 无 u8 收窄、无 256 固定上限）。**一台设备一条 route**——多资源维度
//! （`irq_index` / 多 MSI-X vector / shared line / 跨 owner delegation）留到
//! 有真实需求时再加，本次不做。
//!
//! # 投递纪律
//!
//! route 在锁内只取一份 `(owner, handler, ctx)` 拷贝，实际回调在**锁外**执行
//! （trap 可能重入，spin 锁不可重入）。回调在 Core 建立的 IRQ 归属作用域内运行
//! （principal = 线 owner，task = None），见 `crate::irq::on_irq`。
//!
//! # 锁序
//!
//! device → irq：**route 的插入 / 移除都在 device 锁内完成**——owner 校验通过
//! 与 route 生效之间不允许插入"设备被并发 release"的窗口。

use super::{RequestContext, ResourceKind};
use crate::component::ComponentId;
use crate::irq::IrqSaveGuard;
use crate::machine::{self, DeviceId};
use crate::trace::{TraceEvent, emit};
use alloc::boxed::Box;
use alloc::vec;
use arch::InterruptController;
use spin::{Mutex, Once};

/// 组件提供的中断处理函数（KernelNative：direct call）。
///
/// `ctx` 原样回传，Core 不解引用——与 endpoint 的 provider `ctx` 同一
/// 生命周期契约（provider Ready 期间有效）。
pub use crate::generated::abi::IrqHandler;

/// 一条 IRQ route 的 Core 真相。
#[derive(Clone, Copy)]
struct IrqRoute {
    owner: ComponentId,
    number: u32,
    /// 函数地址与 context 以 `usize` 保存，避免把裸指针放进全局 `Send` 容器。
    handler: usize,
    ctx: usize,
}

/// 注册 / 使能 / 释放失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqError {
    /// `DeviceId` 越界 / 机器信息未提交（正常组件运行期不可达）。
    DeviceNotFound,
    /// 设备没有中断线（FDT 无 `interrupts` / `irq == None`）。
    NoIrq,
    /// caller 不是该设备 owner（IRQ 只能由设备 owner 注册）。
    NotOwner,
    /// 尚未注册 handler 就 enable / 释放一个不存在的 route。
    NoHandler,
}

/// IRQ route 真相表：按已提交快照的设备数定容（一个设备最多一条 route）。
pub struct IrqTable {
    routes: Box<[Option<IrqRoute>]>,
}

impl IrqTable {
    /// 建立恰好 `devices` 个空 route 槽位的表（快照设备数；空表合法）。
    pub fn new(devices: usize) -> Self {
        Self {
            routes: vec![None; devices].into_boxed_slice(),
        }
    }

    /// 槽位数 = 已提交快照的设备数。
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    fn slot(&self, device: DeviceId) -> Option<&Option<IrqRoute>> {
        self.routes.get(device.raw() as usize)
    }

    fn slot_mut(&mut self, device: DeviceId) -> Option<&mut Option<IrqRoute>> {
        self.routes.get_mut(device.raw() as usize)
    }

    /// 注册 / 替换该设备的投递目标；越界 id → `DeviceNotFound`（不 panic）。
    pub fn register(
        &mut self,
        owner: ComponentId,
        device: DeviceId,
        number: u32,
        handler: IrqHandler,
        ctx: *mut (),
    ) -> Result<(), IrqError> {
        let Some(slot) = self.slot_mut(device) else {
            return Err(IrqError::DeviceNotFound);
        };
        *slot = Some(IrqRoute {
            owner,
            number,
            handler: handler as usize,
            ctx: ctx as usize,
        });
        Ok(())
    }

    /// 该设备是否已注册 route（供 device 释放的子项检查）。
    pub fn has_route_for_device(&self, device: DeviceId) -> bool {
        self.slot(device).is_some_and(Option::is_some)
    }

    /// 该设备 route 的中断号（enable/disable 锁外用）。
    pub fn number_for(&self, device: DeviceId) -> Option<u32> {
        self.slot(device)
            .and_then(|route| route.as_ref())
            .map(|route| route.number)
    }

    /// 撤销一条 route：非 owner / 不存在 → 错误。
    pub fn release(&mut self, owner: ComponentId, device: DeviceId) -> Result<(), IrqError> {
        let Some(slot) = self.slot_mut(device) else {
            return Err(IrqError::DeviceNotFound);
        };
        match slot {
            Some(route) if route.owner == owner => {
                *slot = None;
                Ok(())
            }
            Some(_) => Err(IrqError::NotOwner),
            None => Err(IrqError::NoHandler),
        }
    }

    /// 撤销 owner 的全部 route（失败路径）。
    pub fn revoke_owner(&mut self, owner: ComponentId) {
        for slot in self.routes.iter_mut() {
            if slot.as_ref().is_some_and(|route| route.owner == owner) {
                *slot = None;
            }
        }
    }

    /// 按中断号取投递目标（trap 上下文）：`(owner, handler, ctx)`。
    pub fn route_of(&self, number: u32) -> Option<(ComponentId, IrqHandler, *mut ())> {
        self.routes.iter().flatten().find_map(|route| {
            (route.number == number).then_some((
                route.owner,
                // SAFETY: handler 只由 `register` 从真实 `IrqHandler` 写入
                // （信任 KernelNative 函数地址，同 scheduler vtable）。
                unsafe { core::mem::transmute::<usize, IrqHandler>(route.handler) },
                route.ctx as *mut (),
            ))
        })
    }
}

// —— 全局（`resource::init` 初始化；测试用 `IrqTable::new()`）——

static TABLE: Once<Mutex<IrqTable>> = Once::new();

/// 测试专用的全局表覆盖（见 [`install_for_test`]）。
#[cfg(test)]
static TEST_TABLE: Mutex<Option<&'static Mutex<IrqTable>>> = Mutex::new(None);

/// 初始化全局 IRQ route 表（`resource::init` 调用一次）：按已提交快照的设备数定容。
pub fn init() {
    let devices = crate::machine::committed().map_or(0, |info| info.devices.len());
    TABLE.call_once(|| Mutex::new(IrqTable::new(devices)));
}

/// **仅测试**：用恰好 `devices` 个槽位的全新表覆盖全局读路径。
#[cfg(test)]
pub(crate) fn install_for_test(devices: usize) {
    let table: &'static Mutex<IrqTable> = Box::leak(Box::new(Mutex::new(IrqTable::new(devices))));
    *TEST_TABLE.lock() = Some(table);
}

/// 取全局 IRQ route 表（init 后可用）。
pub fn get_table() -> &'static Mutex<IrqTable> {
    #[cfg(test)]
    if let Some(table) = *TEST_TABLE.lock() {
        return table;
    }
    TABLE.get().expect("irq table not initialized")
}

/// 该设备上是否还有 live IRQ route（供 `device::release` 的子项检查）。
pub fn has_route_for_device(device: DeviceId) -> bool {
    get_table().lock().has_route_for_device(device)
}

/// 解析 `DeviceId` → 中断号。
fn resolve(device: DeviceId) -> Result<u32, IrqError> {
    let Some(machine) = machine::committed() else {
        return Err(IrqError::DeviceNotFound);
    };
    let Some(descriptor) = machine.devices.get(device.raw() as usize) else {
        return Err(IrqError::DeviceNotFound);
    };
    descriptor.irq.ok_or(IrqError::NoIrq)
}

/// 注册该设备的中断投递目标。
///
/// 只有设备 owner 能注册（Core 验证 device 表 owner，不信任组件自报身份）。
/// **device 锁在 route 插入期间保持持有**（device → irq 锁序）：owner 校验通过
/// 与 route 生效之间没有可插入的释放窗口。
pub fn register(
    ctx: &RequestContext,
    device: DeviceId,
    handler: IrqHandler,
    handler_ctx: *mut (),
) -> Result<(), IrqError> {
    let number = resolve(device)?;
    let _guard = IrqSaveGuard::new();
    let device_table = super::device::get_table().lock();
    if device_table.owner(device) != Some(ctx.component) {
        return Err(IrqError::NotOwner);
    }
    get_table()
        .lock()
        .register(ctx.component, device, number, handler, handler_ctx)?;
    drop(device_table);
    emit(TraceEvent::ResourceGrant {
        component: ctx.component,
        kind: ResourceKind::Irq,
        id: u64::from(device.raw()),
    });
    Ok(())
}

/// 使能该设备的中断线：先验证已注册 route，再配置中断控制器。
///
/// 表锁只覆盖验证；PLIC 寄存器与 CPU 使能位在**锁外**写（MMIO 慢，trap 可重入）。
pub fn enable(ctx: &RequestContext, device: DeviceId) -> Result<(), IrqError> {
    resolve(device)?;
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        let table = get_table().lock();
        table.number_for(device).ok_or(IrqError::NoHandler)?
    };
    // 只开控制器上的**这条线**。本 CPU 的外部中断投递源与全局闸门在
    // `irq::init`（`InterruptController::init_cpu`）与 boot 的 `enable_irq` 里
    // 处理——`enable(line)` 不得在**任意调用者 CPU** 上开本地投递。
    arch::InterruptImpl::enable(number);
    Ok(())
}

/// 关断该设备的中断线（控制器层）。
pub fn disable(ctx: &RequestContext, device: DeviceId) -> Result<(), IrqError> {
    resolve(device)?;
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        let table = get_table().lock();
        table.number_for(device).ok_or(IrqError::NoHandler)?
    };
    // 锁外关线：trap 可重入、控制器写慢。
    arch::InterruptImpl::disable(number);
    Ok(())
}

/// 释放该设备的 IRQ route：先撤销 route（此后不再投递给已死 owner），
/// 再在锁外关断控制器上的线。route 移除在 device 锁内完成（锁序 device → irq）。
pub fn release(ctx: &RequestContext, device: DeviceId) -> Result<(), IrqError> {
    resolve(device)?;
    let _guard = IrqSaveGuard::new();
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        let mut table = get_table().lock();
        let number = table.number_for(device).ok_or(IrqError::NoHandler)?;
        table.release(ctx.component, device)?;
        number
    };
    emit(TraceEvent::ResourceRevoke {
        component: ctx.component,
        kind: ResourceKind::Irq,
        id: u64::from(device.raw()),
    });
    arch::InterruptImpl::disable(number);
    Ok(())
}

/// 撤销 owner 的全部 IRQ route（失败路径）。
///
/// 控制器上的线保持当前状态；该线若已无 owner，重新放行只会投递到无人认领的
/// 线；下一次合法 `enable` 会重新配置它。
pub fn revoke_owner(owner: ComponentId) {
    let _guard = IrqSaveGuard::new();
    get_table().lock().revoke_owner(owner);
}

#[cfg(test)]
mod tests {
    use super::{IrqError, IrqTable};
    use crate::component::ComponentId;
    use crate::machine::DeviceId;

    extern "C" fn handler(_ctx: *mut ()) {}

    fn owner(raw: u32) -> ComponentId {
        ComponentId::from_raw(raw)
    }

    #[test]
    fn register_route_and_dispatch() {
        let a = owner(1);
        let mut table = IrqTable::new(8);

        assert!(!table.has_route_for_device(DeviceId::from_raw(3)));
        assert_eq!(
            table.register(a, DeviceId::from_raw(3), 42, handler, core::ptr::null_mut()),
            Ok(())
        );
        assert!(table.has_route_for_device(DeviceId::from_raw(3)));
        assert_eq!(table.number_for(DeviceId::from_raw(3)), Some(42));

        let (routed_owner, routed_handler, _ctx) = table.route_of(42).expect("route");
        assert_eq!(routed_owner, a);
        routed_handler(core::ptr::null_mut());
        assert!(table.route_of(43).is_none());
    }

    /// 全宽设备身份：route 可锚在 `DeviceId ≥ 256` 上；越界注册返回
    /// `DeviceNotFound`（旧的 256 固定表 / u8 收窄已删除）。
    #[test]
    fn register_covers_device_ids_beyond_a_byte_and_rejects_out_of_range() {
        let a = owner(1);
        let mut table = IrqTable::new(300);
        let far = DeviceId::from_raw(260);

        assert_eq!(
            table.register(a, far, 77, handler, core::ptr::null_mut()),
            Ok(())
        );
        assert_eq!(table.number_for(far), Some(77));
        assert!(table.route_of(77).is_some());

        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(300),
                1,
                handler,
                core::ptr::null_mut()
            ),
            Err(IrqError::DeviceNotFound)
        );
        assert_eq!(table.number_for(DeviceId::from_raw(300)), None);
    }

    #[test]
    fn release_is_scoped_to_owner() {
        let a = owner(1);
        let b = owner(2);
        let mut table = IrqTable::new(8);
        table
            .register(a, DeviceId::from_raw(3), 42, handler, core::ptr::null_mut())
            .unwrap();

        assert_eq!(
            table.release(b, DeviceId::from_raw(3)),
            Err(IrqError::NotOwner)
        );
        assert_eq!(table.release(a, DeviceId::from_raw(3)), Ok(()));
        assert!(!table.has_route_for_device(DeviceId::from_raw(3)));
        assert_eq!(
            table.release(a, DeviceId::from_raw(3)),
            Err(IrqError::NoHandler)
        );
    }

    #[test]
    fn revoke_owner_clears_only_its_routes() {
        let a = owner(1);
        let b = owner(2);
        let mut table = IrqTable::new(8);
        table
            .register(a, DeviceId::from_raw(3), 42, handler, core::ptr::null_mut())
            .unwrap();
        table
            .register(b, DeviceId::from_raw(4), 43, handler, core::ptr::null_mut())
            .unwrap();

        table.revoke_owner(a);

        assert!(!table.has_route_for_device(DeviceId::from_raw(3)));
        assert!(table.has_route_for_device(DeviceId::from_raw(4)));
    }
}
