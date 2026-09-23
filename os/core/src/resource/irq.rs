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
//! trap → on_external → route(number) → 锁外调用组件 handler
//!   ▼
//! kcore_irq_disable/release(device_id)
//! ```
//!
//! 锚点是 **DeviceId**，不是 `IrqHandle`：`DeviceDescriptor` 本身带 `irq`，
//! 单 IRQ 设备下再套一层"MMIO → IRQ authority 派生"没有真实用途，已删除。
//!
//! # 刻意不做（defer）
//!
//! - **Polled / event delivery**（`register_polled` / `poll` / `ack`）：那是为
//!   未来隔离域设计的 event/wake 机制，执行模型尚未定稿。KernelNative 当前只走
//!   最简单路径：IRQ → Core route → native callback（trap 内同步调用）。
//! - 多 MSI-X vector / shared line / 跨 owner delegation：真实需求出现再加
//!   `irq_index` 或动态 IRQ 身份。
//!
//! # 投递纪律
//!
//! route 在锁内只取一份 `(owner, handler, ctx)` 拷贝，实际回调在**锁外**执行
//! （trap 可能重入，spin 锁不可重入）。回调在 Core 建立的 IRQ 归属作用域内运行
//! （principal = 线 owner，task = None），见 `crate::irq::on_external`。

use super::{RequestContext, ResourceKind};
use crate::component::ComponentId;
use crate::irq::IrqSaveGuard;
use crate::machine::{self, DeviceId};
use crate::trace::{TraceEvent, emit};
use arch::InterruptController;
use spin::{Mutex, Once};

/// 组件提供的中断处理函数（phase 1 KernelNative：direct call）。
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

/// IRQ route 真相表（按 `device_index` 锚定：单 IRQ 设备最多一条 route）。
pub struct IrqTable {
    routes: [Option<IrqRoute>; 256],
}

impl IrqTable {
    pub const fn new() -> Self {
        Self {
            routes: [None; 256],
        }
    }

    fn index(device_index: u8) -> usize {
        device_index as usize
    }

    /// 注册 / 替换该设备的投递目标。
    pub fn register(
        &mut self,
        owner: ComponentId,
        device_index: u8,
        number: u32,
        handler: IrqHandler,
        ctx: *mut (),
    ) {
        self.routes[Self::index(device_index)] = Some(IrqRoute {
            owner,
            number,
            handler: handler as usize,
            ctx: ctx as usize,
        });
    }

    /// 该设备是否已注册 route（供 device 释放的子项检查）。
    pub fn has_route_for_device(&self, device_index: u8) -> bool {
        self.routes[Self::index(device_index)].is_some()
    }

    /// 该设备 route 的中断号（enable/disable 锁外用）。
    pub fn number_for(&self, device_index: u8) -> Option<u32> {
        self.routes[Self::index(device_index)].map(|route| route.number)
    }

    /// 撤销一条 route：非 owner / 不存在 → 错误。
    pub fn release(&mut self, owner: ComponentId, device_index: u8) -> Result<(), IrqError> {
        match self.routes[Self::index(device_index)] {
            Some(route) if route.owner == owner => {
                self.routes[Self::index(device_index)] = None;
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
                // （phase 1 信任 KernelNative 函数地址，同 scheduler vtable）。
                unsafe { core::mem::transmute::<usize, IrqHandler>(route.handler) },
                route.ctx as *mut (),
            ))
        })
    }
}

impl Default for IrqTable {
    fn default() -> Self {
        Self::new()
    }
}

// —— 全局（`resource::init` 初始化；测试用 `IrqTable::new()`）——

static TABLE: Once<Mutex<IrqTable>> = Once::new();

/// 初始化全局 IRQ route 表（`resource::init` 调用一次）。
pub fn init() {
    TABLE.call_once(|| Mutex::new(IrqTable::new()));
}

/// 取全局 IRQ route 表（init 后可用）。
pub fn get_table() -> &'static Mutex<IrqTable> {
    TABLE.get().expect("irq table not initialized")
}

/// 该设备上是否还有 live IRQ route（供 `device::release` 的子项检查）。
pub fn has_route_for_device(device_index: u8) -> bool {
    get_table().lock().has_route_for_device(device_index)
}

/// 解析 `DeviceId` → `(device_index, irq number)`。
fn resolve(device: DeviceId) -> Result<(u8, u32), IrqError> {
    let Some(machine) = machine::committed() else {
        return Err(IrqError::DeviceNotFound);
    };
    let Some(descriptor) = machine.devices[..machine.dev_count].get(device.raw() as usize) else {
        return Err(IrqError::DeviceNotFound);
    };
    let Some(number) = descriptor.irq else {
        return Err(IrqError::NoIrq);
    };
    let device_index = u8::try_from(device.raw()).map_err(|_| IrqError::DeviceNotFound)?;
    Ok((device_index, number))
}

/// 注册该设备的中断投递目标。
///
/// 只有设备 owner 能注册（Core 验证 device 表 owner，不信任组件自报身份）。
pub fn register(
    ctx: &RequestContext,
    device: DeviceId,
    handler: IrqHandler,
    handler_ctx: *mut (),
) -> Result<(), IrqError> {
    let (device_index, number) = resolve(device)?;
    let _guard = IrqSaveGuard::new();
    // 锁序 device → irq（device 表是最外层）。
    let device_table = super::device::get_table().lock();
    if device_table.owner(device_index) != Some(ctx.component) {
        return Err(IrqError::NotOwner);
    }
    drop(device_table);
    get_table()
        .lock()
        .register(ctx.component, device_index, number, handler, handler_ctx);
    emit(TraceEvent::ResourceGrant {
        component: ctx.component,
        kind: ResourceKind::Irq,
        id: u64::from(device_index),
    });
    Ok(())
}

/// 使能该设备的中断线：先验证已注册 route，再配置中断控制器。
///
/// 表锁只覆盖验证；PLIC 寄存器与 CPU 使能位在**锁外**写（MMIO 慢，trap 可重入）。
pub fn enable(ctx: &RequestContext, device: DeviceId) -> Result<(), IrqError> {
    let (device_index, _) = resolve(device)?;
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device_index) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        drop(device_table);
        let table = get_table().lock();
        table.number_for(device_index).ok_or(IrqError::NoHandler)?
    };
    // 先开控制器上的线，再开 CPU 闸门（顺序反了容易吃到伪中断）。
    arch::InterruptImpl::enable(number);
    arch::InterruptImpl::enable_external_interrupt();
    Ok(())
}

/// 关断该设备的中断线（控制器层）。
pub fn disable(ctx: &RequestContext, device: DeviceId) -> Result<(), IrqError> {
    let (device_index, _) = resolve(device)?;
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device_index) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        drop(device_table);
        let table = get_table().lock();
        table.number_for(device_index).ok_or(IrqError::NoHandler)?
    };
    // 锁外关线：trap 可重入、控制器写慢。
    arch::InterruptImpl::disable(number);
    Ok(())
}

/// 释放该设备的 IRQ route：先撤销 route（此后不再投递给已死 owner），
/// 再在锁外关断控制器上的线。
pub fn release(ctx: &RequestContext, device: DeviceId) -> Result<(), IrqError> {
    let (device_index, _) = resolve(device)?;
    let _guard = IrqSaveGuard::new();
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device_index) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        drop(device_table);
        let mut table = get_table().lock();
        let number = table.number_for(device_index).ok_or(IrqError::NoHandler)?;
        table.release(ctx.component, device_index)?;
        number
    };
    emit(TraceEvent::ResourceRevoke {
        component: ctx.component,
        kind: ResourceKind::Irq,
        id: u64::from(device_index),
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

    extern "C" fn handler(_ctx: *mut ()) {}

    fn owner(raw: u32) -> ComponentId {
        ComponentId::from_raw(raw)
    }

    #[test]
    fn register_route_and_dispatch() {
        let a = owner(1);
        let mut table = IrqTable::new();

        assert!(!table.has_route_for_device(3));
        table.register(a, 3, 42, handler, core::ptr::null_mut());
        assert!(table.has_route_for_device(3));
        assert_eq!(table.number_for(3), Some(42));

        let (routed_owner, routed_handler, _ctx) = table.route_of(42).expect("route");
        assert_eq!(routed_owner, a);
        routed_handler(core::ptr::null_mut());
        assert!(table.route_of(43).is_none());
    }

    #[test]
    fn release_is_scoped_to_owner() {
        let a = owner(1);
        let b = owner(2);
        let mut table = IrqTable::new();
        table.register(a, 3, 42, handler, core::ptr::null_mut());

        assert_eq!(table.release(b, 3), Err(IrqError::NotOwner));
        assert_eq!(table.release(a, 3), Ok(()));
        assert!(!table.has_route_for_device(3));
        assert_eq!(table.release(a, 3), Err(IrqError::NoHandler));
    }

    #[test]
    fn revoke_owner_clears_only_its_routes() {
        let a = owner(1);
        let b = owner(2);
        let mut table = IrqTable::new();
        table.register(a, 3, 42, handler, core::ptr::null_mut());
        table.register(b, 4, 43, handler, core::ptr::null_mut());

        table.revoke_owner(a);

        assert!(!table.has_route_for_device(3));
        assert!(table.has_route_for_device(4));
    }
}
