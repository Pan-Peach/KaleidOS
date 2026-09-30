//! IRQ routes：device → 中断线的 Core 真相（二维 `(DeviceId, resource_index)`）。
//!
//! # 主线
//!
//! ```text
//! claimed DeviceId + resource_index（该设备第几条中断资源）
//!   │  kcore_irq_register(device_id, resource_index, handler, ctx)
//!   │    Core: 验证 owner → 查 MachineInfo.devices[id].interrupts[index].line → 记 route
//!   ▼
//! kcore_irq_enable(device_id, resource_index) → 配置中断控制器（arch）
//! trap → on_irq（后端 ack/映射后）→ route(logical_line) → 锁外调用组件 handler
//!   ▼
//! kcore_irq_disable/release(device_id, resource_index)
//! ```
//!
//! 锚点是 **`(DeviceId, resource_index)`**，不是 `IrqHandle`：`DeviceDescriptor`
//! 本身带完整中断资源列表（每条资源一个固件 specifier + 可选的逻辑行号），
//! 一台设备可以有多条中断——二维 key 就是"哪台设备的哪条资源"。
//!
//! route 表**按已提交快照定容**（外层按设备、内层按该设备的中断资源数；
//! boxed slice，无 u8 收窄、无 256 固定上限）。**所有槽位在 `resource::init`
//! 一次性分配**；register / trap 投递不做任何分配。
//!
//! # 无 shared-line fanout
//!
//! 同一个逻辑 IRQ 号只允许挂在**一个** `(device, resource_index)` key 下：
//! `route_of` 是 first-match 的线性扫描，若允许两个 key 共享同一条线，
//! 后来者会被静默吞掉。换 key 重复注册 → [`IrqError::LineBusy`]。
//!
//! # trace 身份
//!
//! `(u64::from(device.raw()) << 32) | u64::from(resource_index)`——设备全宽
//! （`u32`）+ 资源下标（`u32`）拼成一个 `u64`，一个整数同时标识两维。
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
    /// 该设备在 `resource_index` 上没有可投递的中断线（下标越界，或固件资源
    /// 存在但未绑定 `line`——例如没有 GIC/PIC 路由，或 PLIC 源不在配置范围）。
    NoIrq,
    /// caller 不是该设备 owner（IRQ 只能由设备 owner 注册）。
    NotOwner,
    /// 尚未注册 handler 就 enable / 释放一个不存在的 route。
    NoHandler,
    /// 该逻辑 IRQ 号已挂在**另一个** `(device, resource_index)` key 下：
    /// 不做 shared-line fanout（first-match 会静默吞掉后来者）。
    LineBusy,
}

/// IRQ route 真相表：外层按 `DeviceId`、内层按该设备的中断资源下标定容。
pub struct IrqTable {
    routes: Box<[Box<[Option<IrqRoute>]>]>,
}

impl IrqTable {
    /// 按快照逐设备分配槽位（`devices[i].interrupts.len()`；空设备 / 空表合法）。
    pub fn new(devices: &[machine::DeviceDescriptor]) -> Self {
        Self {
            routes: devices
                .iter()
                .map(|device| vec![None; device.interrupts.len()].into_boxed_slice())
                .collect::<alloc::vec::Vec<_>>()
                .into_boxed_slice(),
        }
    }

    /// **仅测试**：按显式槽位数构造（生产路径只有 [`IrqTable::new`]，容量来自快照）。
    #[cfg(test)]
    pub(crate) fn from_counts(counts: &[usize]) -> Self {
        Self {
            routes: counts
                .iter()
                .map(|&count| vec![None; count].into_boxed_slice())
                .collect::<alloc::vec::Vec<_>>()
                .into_boxed_slice(),
        }
    }

    /// 外层槽位数 = 已提交快照的设备数。
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    fn slot(&self, device: DeviceId, resource_index: u32) -> Option<&Option<IrqRoute>> {
        self.routes
            .get(device.raw() as usize)?
            .get(resource_index as usize)
    }

    fn slot_mut(&mut self, device: DeviceId, resource_index: u32) -> Option<&mut Option<IrqRoute>> {
        self.routes
            .get_mut(device.raw() as usize)?
            .get_mut(resource_index as usize)
    }

    /// 注册 / 替换该资源上的投递目标；越界 id / 下标 → 错误（不 panic）。
    ///
    /// **同一逻辑线只允许一个资源 key**：若 `number` 已挂在别的 key 下 →
    /// [`IrqError::LineBusy`]（即使 owner 相同也拒绝——`route_of` 是线性
    /// first-match，共享线会静默吞掉后来者）。
    pub fn register(
        &mut self,
        owner: ComponentId,
        device: DeviceId,
        resource_index: u32,
        number: u32,
        handler: IrqHandler,
        ctx: *mut (),
    ) -> Result<(), IrqError> {
        // 先验证 key（外层设备 / 内层资源），再查重复线——越界请求不能因为
        // 扫描顺序被误报成 LineBusy。
        if self.slot(device, resource_index).is_none() {
            return Err(self.index_error(device));
        }
        for (device_index, slots) in self.routes.iter().enumerate() {
            for (index, route) in slots.iter().enumerate() {
                let Some(route) = route else { continue };
                let same_key =
                    device_index == device.raw() as usize && index == resource_index as usize;
                if route.number == number && !same_key {
                    return Err(IrqError::LineBusy);
                }
            }
        }
        let Some(slot) = self.slot_mut(device, resource_index) else {
            // 上面刚验证过，走不到这里；保持不 panic 的形状。
            return Err(self.index_error(device));
        };
        *slot = Some(IrqRoute {
            owner,
            number,
            handler: handler as usize,
            ctx: ctx as usize,
        });
        Ok(())
    }

    /// 越界区分：外层（设备）越界 → `DeviceNotFound`，内层（资源）越界 → `NoIrq`。
    fn index_error(&self, device: DeviceId) -> IrqError {
        if self.routes.get(device.raw() as usize).is_some() {
            IrqError::NoIrq
        } else {
            IrqError::DeviceNotFound
        }
    }

    /// 该设备是否还有任何 live route（供 device 释放的子项检查；覆盖全部资源）。
    pub fn has_route_for_device(&self, device: DeviceId) -> bool {
        self.routes
            .get(device.raw() as usize)
            .is_some_and(|slots| slots.iter().any(Option::is_some))
    }

    /// 该资源 route 的中断号（enable/disable 锁外用）。
    pub fn number_for(&self, device: DeviceId, resource_index: u32) -> Option<u32> {
        self.slot(device, resource_index)
            .and_then(|route| route.as_ref())
            .map(|route| route.number)
    }

    /// 撤销一条 route：非 owner / 不存在 / 越界 → 错误。
    pub fn release(
        &mut self,
        owner: ComponentId,
        device: DeviceId,
        resource_index: u32,
    ) -> Result<(), IrqError> {
        let Some(slot) = self.slot_mut(device, resource_index) else {
            return Err(self.index_error(device));
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

    /// 撤销 owner 的全部 route（失败路径；覆盖所有设备与资源）。
    pub fn revoke_owner(&mut self, owner: ComponentId) {
        for slots in self.routes.iter_mut() {
            for slot in slots.iter_mut() {
                if slot.as_ref().is_some_and(|route| route.owner == owner) {
                    *slot = None;
                }
            }
        }
    }

    /// 按中断号取投递目标（trap 上下文）：`(owner, handler, ctx)`。
    pub fn route_of(&self, number: u32) -> Option<(ComponentId, IrqHandler, *mut ())> {
        self.routes.iter().flatten().flatten().find_map(|route| {
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

// —— 全局（`resource::init` 初始化；测试用 `IrqTable::new()` / `install_for_test`）——

static TABLE: Once<Mutex<IrqTable>> = Once::new();

/// 测试专用的全局表覆盖（见 [`install_for_test`]）。
#[cfg(test)]
static TEST_TABLE: Mutex<Option<&'static Mutex<IrqTable>>> = Mutex::new(None);

/// 初始化全局 IRQ route 表（`resource::init` 调用一次）：按已提交快照定容
/// （外层设备数 × 内层中断资源数）。
pub fn init() {
    let table =
        IrqTable::new(crate::machine::committed().map_or(&[], |info| info.devices.as_ref()));
    TABLE.call_once(|| Mutex::new(table));
}

/// **仅测试**：用按 `counts`（逐设备资源数）定容的全新表覆盖全局读路径。
#[cfg(test)]
pub(crate) fn install_for_test(counts: &[usize]) {
    let table: &'static Mutex<IrqTable> =
        Box::leak(Box::new(Mutex::new(IrqTable::from_counts(counts))));
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

/// trace 身份：设备全宽 + 资源下标拼成一个 `u64`。
fn trace_id(device: DeviceId, resource_index: u32) -> u64 {
    (u64::from(device.raw()) << 32) | u64::from(resource_index)
}

/// 解析 `(DeviceId, resource_index)` → 逻辑中断号。
///
/// 设备越界 / 未提交 → `DeviceNotFound`；资源下标越界或 `line: None` → `NoIrq`。
fn resolve(device: DeviceId, resource_index: u32) -> Result<u32, IrqError> {
    let Some(machine) = machine::committed() else {
        return Err(IrqError::DeviceNotFound);
    };
    let Some(descriptor) = machine.devices.get(device.raw() as usize) else {
        return Err(IrqError::DeviceNotFound);
    };
    let Some(resource) = descriptor.interrupts.get(resource_index as usize) else {
        return Err(IrqError::NoIrq);
    };
    resource.line.ok_or(IrqError::NoIrq)
}

/// 注册该设备某条中断资源的投递目标。
///
/// 只有设备 owner 能注册（Core 验证 device 表 owner，不信任组件自报身份）。
/// **device 锁在 route 插入期间保持持有**（device → irq 锁序）：owner 校验通过
/// 与 route 生效之间没有可插入的释放窗口。
pub fn register(
    ctx: &RequestContext,
    device: DeviceId,
    resource_index: u32,
    handler: IrqHandler,
    handler_ctx: *mut (),
) -> Result<(), IrqError> {
    let number = resolve(device, resource_index)?;
    let _guard = IrqSaveGuard::new();
    let device_table = super::device::get_table().lock();
    if device_table.owner(device) != Some(ctx.component) {
        return Err(IrqError::NotOwner);
    }
    get_table().lock().register(
        ctx.component,
        device,
        resource_index,
        number,
        handler,
        handler_ctx,
    )?;
    drop(device_table);
    emit(TraceEvent::ResourceGrant {
        component: ctx.component,
        kind: ResourceKind::Irq,
        id: trace_id(device, resource_index),
    });
    Ok(())
}

/// 使能该设备某条中断资源：先验证已注册 route，再配置中断控制器。
///
/// 表锁只覆盖验证；PLIC 寄存器与 CPU 使能位在**锁外**写（MMIO 慢，trap 可重入）。
pub fn enable(ctx: &RequestContext, device: DeviceId, resource_index: u32) -> Result<(), IrqError> {
    resolve(device, resource_index)?;
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        let table = get_table().lock();
        table
            .number_for(device, resource_index)
            .ok_or(IrqError::NoHandler)?
    };
    // 只开控制器上的**这条线**。本 CPU 的外部中断投递源与全局闸门在
    // `irq::init`（`InterruptController::init_cpu`）与 boot 的 `enable_irq` 里
    // 处理——`enable(line)` 不得在**任意调用者 CPU** 上开本地投递。
    arch::InterruptImpl::enable(number);
    Ok(())
}

/// 关断该设备某条中断资源（控制器层）。
pub fn disable(
    ctx: &RequestContext,
    device: DeviceId,
    resource_index: u32,
) -> Result<(), IrqError> {
    resolve(device, resource_index)?;
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        let table = get_table().lock();
        table
            .number_for(device, resource_index)
            .ok_or(IrqError::NoHandler)?
    };
    // 锁外关线：trap 可重入、控制器写慢。
    arch::InterruptImpl::disable(number);
    Ok(())
}

/// 释放该设备某条中断资源的 route：先撤销 route（此后不再投递给已死 owner），
/// 再在锁外关断控制器上的线。route 移除在 device 锁内完成（锁序 device → irq）。
pub fn release(
    ctx: &RequestContext,
    device: DeviceId,
    resource_index: u32,
) -> Result<(), IrqError> {
    resolve(device, resource_index)?;
    let _guard = IrqSaveGuard::new();
    let number = {
        let device_table = super::device::get_table().lock();
        if device_table.owner(device) != Some(ctx.component) {
            return Err(IrqError::NotOwner);
        }
        let mut table = get_table().lock();
        let number = table
            .number_for(device, resource_index)
            .ok_or(IrqError::NoHandler)?;
        table.release(ctx.component, device, resource_index)?;
        number
    };
    emit(TraceEvent::ResourceRevoke {
        component: ctx.component,
        kind: ResourceKind::Irq,
        id: trace_id(device, resource_index),
    });
    arch::InterruptImpl::disable(number);
    Ok(())
}

/// 撤销 owner 的全部 IRQ route（失败路径；覆盖所有设备与资源）。
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
    extern "C" fn other_handler(_ctx: *mut ()) {}

    fn owner(raw: u32) -> ComponentId {
        ComponentId::from_raw(raw)
    }

    #[test]
    fn register_route_and_dispatch() {
        let a = owner(1);
        let mut table = IrqTable::from_counts(&[2]);

        assert!(!table.has_route_for_device(DeviceId::from_raw(0)));
        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(0),
                0,
                42,
                handler,
                core::ptr::null_mut()
            ),
            Ok(())
        );
        assert!(table.has_route_for_device(DeviceId::from_raw(0)));
        assert_eq!(table.number_for(DeviceId::from_raw(0), 0), Some(42));

        let (routed_owner, routed_handler, _ctx) = table.route_of(42).expect("route");
        assert_eq!(routed_owner, a);
        routed_handler(core::ptr::null_mut());
        assert!(table.route_of(43).is_none());
    }

    /// 两个中断资源挂在同一台设备上：各自独立注册 / 查询 / 投递 / 释放，
    /// 互不影响（二维 `(DeviceId, resource_index)` 的核心验收）。
    #[test]
    fn two_interrupt_resources_on_one_device_route_independently() {
        let a = owner(1);
        let mut table = IrqTable::from_counts(&[2]);

        assert_eq!(
            table.register(a, DeviceId::from_raw(0), 0, 10, handler, 0x1000 as *mut ()),
            Ok(())
        );
        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(0),
                1,
                11,
                other_handler,
                0x2000 as *mut ()
            ),
            Ok(())
        );
        assert_eq!(table.number_for(DeviceId::from_raw(0), 0), Some(10));
        assert_eq!(table.number_for(DeviceId::from_raw(0), 1), Some(11));

        let (_, first, first_ctx) = table.route_of(10).expect("resource 0 route");
        assert_eq!(first as usize, handler as *const () as usize);
        assert_eq!(first_ctx as usize, 0x1000);
        let (_, second, second_ctx) = table.route_of(11).expect("resource 1 route");
        assert_eq!(second as usize, other_handler as *const () as usize);
        assert_eq!(second_ctx as usize, 0x2000);

        // 释放资源 0 不影响资源 1。
        assert_eq!(table.release(a, DeviceId::from_raw(0), 0), Ok(()));
        assert!(table.route_of(10).is_none());
        assert!(table.route_of(11).is_some());
        assert!(table.has_route_for_device(DeviceId::from_raw(0)));
        assert_eq!(table.number_for(DeviceId::from_raw(0), 1), Some(11));
    }

    /// 全宽设备身份：route 可锚在 `DeviceId ≥ 256` 上；`resource_index` 越界
    /// → `NoIrq`，设备越界 → `DeviceNotFound`（旧的 256 固定表 / u8 收窄已删除）。
    #[test]
    fn register_covers_device_ids_beyond_a_byte_and_rejects_out_of_range() {
        let a = owner(1);
        let mut table = IrqTable::from_counts(&[1; 300]);
        let far = DeviceId::from_raw(260);

        assert_eq!(
            table.register(a, far, 0, 77, handler, core::ptr::null_mut()),
            Ok(())
        );
        assert_eq!(table.number_for(far, 0), Some(77));
        assert!(table.route_of(77).is_some());

        // 资源下标越界：设备存在 → NoIrq（不是 DeviceNotFound）。
        assert_eq!(
            table.register(a, far, 1, 78, handler, core::ptr::null_mut()),
            Err(IrqError::NoIrq)
        );
        assert_eq!(table.number_for(far, 1), None);
        assert_eq!(table.route_of(78), None);

        // 设备越界：DeviceNotFound。
        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(300),
                0,
                1,
                handler,
                core::ptr::null_mut()
            ),
            Err(IrqError::DeviceNotFound)
        );
        assert_eq!(table.number_for(DeviceId::from_raw(300), 0), None);
    }

    /// 同一逻辑线换一个资源 key 注册 → `LineBusy`（不做 shared-line fanout：
    /// `route_of` first-match，重复注册会让后来者永远收不到投递）。
    #[test]
    fn duplicate_logical_line_under_a_different_key_is_rejected() {
        let a = owner(1);
        let mut table = IrqTable::from_counts(&[2, 1]);

        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(0),
                0,
                42,
                handler,
                core::ptr::null_mut()
            ),
            Ok(())
        );
        // 同设备、另一个资源 key。
        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(0),
                1,
                42,
                other_handler,
                core::ptr::null_mut()
            ),
            Err(IrqError::LineBusy)
        );
        // 另一台设备、同一逻辑线。
        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(1),
                0,
                42,
                other_handler,
                core::ptr::null_mut()
            ),
            Err(IrqError::LineBusy)
        );
        // 原 route 未被扰动；同一个 key 的重新注册（handler 替换）允许。
        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(0),
                0,
                42,
                other_handler,
                core::ptr::dangling_mut::<()>(),
            ),
            Ok(())
        );
        let (_, routed, _) = table.route_of(42).expect("route");
        assert_eq!(routed as usize, other_handler as *const () as usize);
        // 释放后该线可被另一个 key 取用。
        assert_eq!(table.release(a, DeviceId::from_raw(0), 0), Ok(()));
        assert_eq!(
            table.register(
                a,
                DeviceId::from_raw(1),
                0,
                42,
                handler,
                core::ptr::null_mut()
            ),
            Ok(())
        );
    }

    #[test]
    fn release_is_scoped_to_owner() {
        let a = owner(1);
        let b = owner(2);
        let mut table = IrqTable::from_counts(&[1; 4]);
        table
            .register(
                a,
                DeviceId::from_raw(3),
                0,
                42,
                handler,
                core::ptr::null_mut(),
            )
            .unwrap();

        assert_eq!(
            table.release(b, DeviceId::from_raw(3), 0),
            Err(IrqError::NotOwner)
        );
        assert_eq!(table.release(a, DeviceId::from_raw(3), 0), Ok(()));
        assert!(!table.has_route_for_device(DeviceId::from_raw(3)));
        assert_eq!(
            table.release(a, DeviceId::from_raw(3), 0),
            Err(IrqError::NoHandler)
        );
    }

    #[test]
    fn revoke_owner_clears_only_its_routes() {
        let a = owner(1);
        let b = owner(2);
        let mut table = IrqTable::from_counts(&[1, 1, 1, 2, 1]);
        table
            .register(
                a,
                DeviceId::from_raw(3),
                0,
                42,
                handler,
                core::ptr::null_mut(),
            )
            .unwrap();
        table
            .register(
                a,
                DeviceId::from_raw(3),
                1,
                43,
                handler,
                core::ptr::null_mut(),
            )
            .unwrap();
        table
            .register(
                b,
                DeviceId::from_raw(4),
                0,
                44,
                handler,
                core::ptr::null_mut(),
            )
            .unwrap();

        table.revoke_owner(a);

        assert!(!table.has_route_for_device(DeviceId::from_raw(3)));
        assert_eq!(table.number_for(DeviceId::from_raw(3), 0), None);
        assert_eq!(table.number_for(DeviceId::from_raw(3), 1), None);
        assert!(table.has_route_for_device(DeviceId::from_raw(4)));
        assert_eq!(table.number_for(DeviceId::from_raw(4), 0), Some(44));
    }

    // ------------------------------------------------------------------
    // 模块级路径：resolve 读快照（下标 / line: None / owner 校验）
    // ------------------------------------------------------------------

    use crate::machine::test_support;
    use crate::resource::RequestContext;
    use alloc::boxed::Box;
    use alloc::vec;

    /// 安装一台含两条中断资源的设备：资源 0 未绑定（line None），资源 1 绑定 42。
    fn install_fixture() -> DeviceId {
        use crate::machine::{
            CpuInfo, DeviceDescriptor, HardwareCpuId, InterruptResource, InterruptSpecifier,
            IoSpace, MemoryRegion,
        };

        let device = DeviceDescriptor {
            spaces: vec![IoSpace::Mmio {
                base: 0x1000_0000,
                size: 0x1000,
            }]
            .into_boxed_slice(),
            interrupts: vec![
                InterruptResource {
                    specifier: InterruptSpecifier::Fdt {
                        controller: 3,
                        cells: Box::new([7]),
                    },
                    line: None,
                },
                InterruptResource {
                    specifier: InterruptSpecifier::Fdt {
                        controller: 3,
                        cells: Box::new([42]),
                    },
                    line: Some(42),
                },
            ]
            .into_boxed_slice(),
            compatibles: vec![Box::<str>::from("demo,device")].into_boxed_slice(),
        };
        let info = test_support::snapshot(
            HardwareCpuId::from_raw(0),
            10_000_000,
            vec![CpuInfo {
                boot_cpu: true,
                hardware_id: HardwareCpuId::from_raw(0),
            }],
            vec![MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }],
            vec![device],
        );
        test_support::install(info);
        // device / irq 表按 fixture 形状重建（进程全局表不能按用例重定容）。
        crate::resource::test_support::reinstall();
        DeviceId::from_raw(0)
    }

    /// `resolve` 读的是快照的 `line`：未绑定（None）与越界下标都 `NoIrq`；
    /// 非 owner / 未注册的 enable 分别 `NotOwner` / `NoHandler`。
    #[test]
    fn module_paths_reject_unbound_out_of_range_and_wrong_owner() {
        let _machine = test_support::GUARD.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        let device = install_fixture();
        let a = RequestContext {
            component: owner(1),
            task: None,
        };
        let b = RequestContext {
            component: owner(2),
            task: None,
        };
        crate::resource::device::get_table()
            .lock()
            .claim(a.component, device)
            .expect("fixture device claim");

        // 未绑定 line（资源 0）→ NoIrq。
        assert_eq!(
            super::register(&a, device, 0, handler, core::ptr::null_mut()),
            Err(IrqError::NoIrq)
        );
        // 资源下标越界 → NoIrq。
        assert_eq!(
            super::register(&a, device, 9, handler, core::ptr::null_mut()),
            Err(IrqError::NoIrq)
        );
        // 设备越界 → DeviceNotFound。
        assert_eq!(
            super::register(&a, DeviceId::from_raw(9), 0, handler, core::ptr::null_mut()),
            Err(IrqError::DeviceNotFound)
        );
        // 非 owner → NotOwner（register / enable / release 一致）。
        assert_eq!(
            super::register(&b, device, 1, handler, core::ptr::null_mut()),
            Err(IrqError::NotOwner)
        );
        assert_eq!(super::enable(&b, device, 1), Err(IrqError::NotOwner));
        assert_eq!(super::release(&b, device, 1), Err(IrqError::NotOwner));

        // owner 注册绑定资源成功；enable 前的 release 是 NoHandler。
        assert_eq!(
            super::register(&a, device, 1, handler, core::ptr::null_mut()),
            Ok(())
        );
        assert_eq!(super::enable(&a, device, 0), Err(IrqError::NoIrq));
        assert_eq!(super::release(&a, device, 0), Err(IrqError::NoIrq));

        // 同一 key 的重新注册 = handler 替换（允许）；release 后该线可再被取用。
        assert_eq!(
            super::register(&a, device, 1, other_handler, core::ptr::null_mut()),
            Ok(())
        );
        let (_, routed, _) = super::get_table().lock().route_of(42).expect("route");
        assert_eq!(routed as usize, other_handler as *const () as usize);

        assert_eq!(super::release(&a, device, 1), Ok(()));
        assert_eq!(super::release(&a, device, 1), Err(IrqError::NoHandler));
    }
}
