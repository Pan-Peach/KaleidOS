//! 组件失败的 Core 编排：标记 Failed，回收它的资源归属，失效它提供的 endpoint。
//!
//! 落地 `docs/architecture/component-model.md` §4.9 的 `fail_component`（最小版）与
//! `docs/architecture/driver-model.md` §7 的撤销不变式：**组件失败 = 逻辑死亡、物理驻留**。
//! 顺序固定：先提交状态真相（Failed），再撤销它持有的资源归属，最后清掉它作为
//! provider 的全部 endpoint（含未提交的 pending publications）。
//!
//! # 与优雅停止的分工
//!
//! - **失败路径（本模块）刻意不调用 `kcomp_instance_destroy`**：失败的组件不值得
//!   信任，Linux 也不对崩溃模块执行 `module_exit`——Core 直接收回资源。代价：
//!   组件侧的设备收尾（stop DMA / reset / mask IRQ）不会发生，Core 的
//!   revoke + device quarantine 是唯一兜底。
//! - **优雅停止（`component/exit.rs::stop_component`）**先信任组件的
//!   `kcomp_instance_destroy` 自行收尾，再调用本模块共享的
//!   [`revoke_authority_and_unbind`] 兜底。
//!
//! # 当前边界
//!
//! - 不强制停止失败组件的任务（只有 yield/exit）。
//! - 物理组件镜像不回收（logical death / physical residency）。

use crate::component::load::ComponentLoadError;
use crate::component::{ComponentId, endpoint, registry};
use crate::resource::{device, dma, irq};

/// 组件失败（逻辑死亡）的 Core 编排：`mark_failed` → 资源兜底。
///
/// **设备 quarantine**：撤销 device ownership 不等于设备可被下一个驱动安全复用
/// （设备可能仍被硬件引用 / 未静默）。因此回收前先把失败组件占用的每个 device
/// quarantine——之后普通认领返回 `-EBUSY`，直到 reboot。组件**优雅、协作式
/// quiesce** 后的 `release` 不进入 quarantine，设备仍可复用。
pub fn fail_component(id: ComponentId, reason: ComponentLoadError) {
    let _ = reason;
    registry::get_registry().lock().mark_failed(id).ok();
    revoke_authority_and_unbind(id);
}

/// Core 兜底：收回组件剩余的资源归属并失效它提供的 endpoint。
///
/// 精确序列（失败路径与优雅停止路径**共用**）：
/// 1. IRQ：撤销 route（投递目标随之消失）；
/// 2. DMA：撤销 mapping，backing lease 进 QUARANTINE（不 free，设备可能仍在 DMA）；
/// 3. Device：撤销 ownership 并把设备标进失败 quarantine；
/// 4. Endpoint：provider 的全部 endpoint 永久失效（tombstone，id 不复用），
///    丢弃 staged pending publications。
///
/// 调用方负责状态提交（失败 = `Failed`；优雅停止 = 随后 `Stopping → Stopped`）。
pub(crate) fn revoke_authority_and_unbind(id: ComponentId) {
    irq::revoke_owner(id);
    dma::revoke_owner(id);
    device::quarantine_owner(id);
    let mut endpoints = endpoint::get_endpoints().lock();
    // endpoint 真相：provider 的全部 endpoint 永久失效（绝不重定向到新实例）。
    endpoints.invalidate_provider(id);
    // staged endpoint publish：pending 全丢弃，旧 provider 完全不受影响。
    endpoints.discard_pending(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentState;
    use crate::component::abi::{InterfaceAbi, InterfaceKind};
    use crate::component::endpoint::{ContractId, EndpointError, EndpointState, ExecutionDomain};
    use crate::component::registry;
    use crate::machine::{DeviceDescriptor, IoSpace};
    use crate::resource::{RequestContext, device, dma, irq};

    extern "C" fn demo_irq(_ctx: *mut ()) {}

    /// 一份伪造 loaded image（无 backing）与 Ready 组件声明辅助。
    fn test_loaded() -> crate::component::loader::LoadedComponent {
        crate::component::registry::test_support::test_loaded(0, None)
    }

    fn declare(reg: &mut registry::Registry, name: &[u8]) -> ComponentId {
        reg.declare(name, test_loaded(), ExecutionDomain::KernelNative)
            .unwrap()
    }

    fn commit_device(device_index: usize, compatible: &str) {
        use crate::machine::{
            CpuInfo, HardwareCpuId, InterruptResource, InterruptSpecifier, MemoryRegion,
        };
        let mut devices = alloc::vec![DeviceDescriptor::empty(); device_index + 1];
        devices[device_index] = DeviceDescriptor {
            spaces: alloc::vec![IoSpace::Mmio {
                base: 0x1000_0000 + device_index * 0x1000,
                size: 0x1000,
            }]
            .into_boxed_slice(),
            interrupts: alloc::vec![InterruptResource {
                specifier: InterruptSpecifier::Isa { line: 8 },
                line: Some(8),
            }]
            .into_boxed_slice(),
            compatibles: alloc::vec![alloc::boxed::Box::<str>::from(compatible)].into_boxed_slice(),
        };
        let info = crate::machine::test_support::snapshot(
            HardwareCpuId::from_raw(0),
            core::num::NonZeroU64::new(10_000_000),
            alloc::vec![CpuInfo {
                boot_cpu: true,
                hardware_id: HardwareCpuId::from_raw(0),
            }],
            alloc::vec![MemoryRegion {
                base: 0x8000_0000,
                size: 0x1000_0000,
            }],
            devices,
        );
        crate::machine::test_support::install(info);
        // device / irq 表按 fixture 尺寸重建（进程全局表不能按用例重定容）。
        crate::resource::test_support::reinstall();
    }

    /// 失败编排：资源回收 + 提交 Failed + 设备 quarantine。
    #[test]
    fn fail_component_revokes_resources() {
        let _machine = crate::machine::test_support::GUARD.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        endpoint::init();
        crate::resource::init();
        commit_device(24, "fail,mmio");

        let id = {
            let mut reg = registry::get_registry().lock();
            let id = declare(&mut reg, b"fail_demo");
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };
        let ctx = RequestContext {
            component: id,
            task: None,
        };
        let device_id = crate::machine::DeviceId::from_raw(24);
        device::claim(&ctx, device_id).expect("claim");
        irq::get_table()
            .lock()
            .register(id, device_id, 0, 8, demo_irq, core::ptr::null_mut())
            .unwrap();
        let buffer = dma::alloc(id, 4096).expect("dma alloc");
        let mapping = dma::map(
            &ctx,
            device_id,
            buffer.ptr,
            buffer.len,
            dma::DmaDirection::ToDevice,
        )
        .expect("dma map");
        registry::get_registry().lock().finish_start(id).unwrap();

        let before_quarantine = dma::quarantine_len();
        fail_component(id, ComponentLoadError::CreateFailed(1));

        // 资源归属被回收。
        assert!(irq::get_table().lock().route_of(8).is_none());
        assert_eq!(dma::unmap(mapping.id), Err(dma::DmaError::NotFound));
        assert_eq!(
            dma::quarantine_len(),
            before_quarantine + 1,
            "DMA backing 必须进 quarantine"
        );
        assert!(!device::get_table().lock().owner(device_id).is_some());
        assert!(device::get_table().lock().is_quarantined(device_id));

        // quarantine 后普通认领 -EBUSY。
        let claimant = RequestContext {
            component: registry::test_support::ready(b"quarantine-claimant"),
            task: None,
        };
        assert_eq!(
            device::claim(&claimant, device_id),
            Err(device::DeviceClaimError::DeviceBusy)
        );

        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Failed
        );

        device::get_table().lock().clear_quarantine();
    }

    /// 声明一个 Ready 组件（endpoint 测试不需要真实 backing）。
    fn ready_instance(reg: &mut registry::Registry) -> ComponentId {
        let id = declare(reg, b"ready_instance");
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    /// 失败路径的 endpoint 兜底：`fail_component` 使失败组件的 endpoint 永久
    /// `Invalid`（dead），未提交的 pending publication 不会在事后浮出；
    /// 其它 provider 的 endpoint 完全不受影响。
    #[test]
    fn fail_component_invalidates_endpoints_permanently() {
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        endpoint::init();
        crate::resource::init();

        const CONTRACT: ContractId = ContractId::from_raw(0xFA11_1001);
        const OTHER_CONTRACT: ContractId = ContractId::from_raw(0xFA11_1002);
        const ENDPOINT_ABI: InterfaceAbi = InterfaceAbi::from_raw(0xFA11_1003);

        // Given：两个 Ready provider；第一个有一条已提交 endpoint + 一条未提交
        // pending，第二个有一条已提交 endpoint。
        let (id, other) = {
            let mut reg = registry::get_registry().lock();
            (ready_instance(&mut reg), ready_instance(&mut reg))
        };
        let (live, other_live) = {
            let reg = registry::get_registry().lock();
            let mut eps = endpoint::get_endpoints().lock();
            eps.stage_publish(
                &reg,
                id,
                b"blk0",
                CONTRACT,
                InterfaceKind::Device,
                ENDPOINT_ABI,
                7,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
            eps.commit_pending(&reg, id).unwrap();
            // 未提交的 pending：create 失败时必须被丢弃。
            eps.stage_publish(
                &reg,
                id,
                b"pending0",
                OTHER_CONTRACT,
                InterfaceKind::Device,
                ENDPOINT_ABI,
                9,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
            eps.stage_publish(
                &reg,
                other,
                b"blk0",
                CONTRACT,
                InterfaceKind::Device,
                ENDPOINT_ABI,
                10,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
            eps.commit_pending(&reg, other).unwrap();
            (
                eps.discover(&reg, id, b"blk0", CONTRACT).unwrap(),
                eps.discover(&reg, other, b"blk0", CONTRACT).unwrap(),
            )
        };

        // When：组件失败（create 失败 / panic / 运行失败共用这条兜底）。
        fail_component(id, ComponentLoadError::CreateFailed(1));

        // Then：失败组件的 endpoint 永久 dead；pending 不可能事后浮出
        // （provider Failed → commit 被 NotReady 拒绝）。
        let reg = registry::get_registry().lock();
        let mut eps = endpoint::get_endpoints().lock();
        assert_eq!(
            eps.lookup(&reg, live, CONTRACT, ENDPOINT_ABI),
            Err(EndpointError::EndpointDead)
        );
        assert_eq!(
            eps.discover(&reg, id, b"blk0", CONTRACT),
            Err(EndpointError::EndpointDead)
        );
        assert_eq!(
            eps.commit_pending(&reg, id),
            Err(EndpointError::ProviderNotReady)
        );
        assert_eq!(
            eps.discover(&reg, id, b"pending0", OTHER_CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );
        // 其它 provider 完全不受影响。
        assert_eq!(
            eps.lookup(&reg, other_live, CONTRACT, ENDPOINT_ABI)
                .unwrap()
                .state,
            EndpointState::Live
        );
    }

    /// Core 兜底（failure / stop 共用）确实**丢弃** staged pending，而不只是让
    /// provider 不可用：provider 仍 Ready 时提交空批不会产出 endpoint。
    #[test]
    fn revoke_backstop_discards_staged_endpoint_publications() {
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        endpoint::init();
        crate::resource::init();

        const CONTRACT: ContractId = ContractId::from_raw(0xFA11_2001);
        const ENDPOINT_ABI: InterfaceAbi = InterfaceAbi::from_raw(0xFA11_2002);

        // Given：一个 Ready provider + 一条 staged pending。
        let id = {
            let mut reg = registry::get_registry().lock();
            ready_instance(&mut reg)
        };
        {
            let reg = registry::get_registry().lock();
            endpoint::get_endpoints()
                .lock()
                .stage_publish(
                    &reg,
                    id,
                    b"pending0",
                    CONTRACT,
                    InterfaceKind::Device,
                    ENDPOINT_ABI,
                    7,
                    core::ptr::null(),
                    core::ptr::null_mut(),
                )
                .unwrap();
        }

        // When：Core 兜底序列（failure / stop 路径共用）。
        revoke_authority_and_unbind(id);

        // Then：provider 仍 Ready，但 pending 已丢弃——commit 空批是 no-op，
        // discover 找不到（若 pending 残留，commit 会产出 endpoint）。
        let reg = registry::get_registry().lock();
        let mut eps = endpoint::get_endpoints().lock();
        assert_eq!(eps.commit_pending(&reg, id), Ok(()));
        assert_eq!(
            eps.discover(&reg, id, b"pending0", CONTRACT),
            Err(EndpointError::EndpointNotFound)
        );
    }
}
