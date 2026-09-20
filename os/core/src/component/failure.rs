//! 组件失败的 Core 编排：标记 Failed，回收它的资源归属，解绑它提供的接口。
//!
//! 落地 `docs/component-model.md` §4.9 的 `fail_component`（最小版）与
//! `docs/driver-model.md` §7 的撤销不变式：**组件失败 = 逻辑死亡、物理驻留**。
//! 顺序固定：先提交状态真相（Failed），再撤销它持有的资源归属，最后清掉它作为
//! provider 的全部 binding（含未提交的 pending publications）。
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
//! # 明确 DEFERRED（本增量不做）
//!
//! - **强制停止失败组件的任务**（当前只有 yield/exit）。
//! - **物理组件镜像回收**：Phase 1 保持 logical death / physical residency。

use crate::component::load::ComponentLoadError;
use crate::component::{ComponentId, interface, registry};
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

/// Core 兜底：收回组件剩余的资源归属并解绑它提供的接口。
///
/// 精确序列（失败路径与优雅停止路径**共用**）：
/// 1. IRQ：撤销 route（投递目标随之消失）；
/// 2. DMA：撤销 mapping，backing lease 进 QUARANTINE（不 free，设备可能仍在 DMA）；
/// 3. Device：撤销 ownership 并把设备标进失败 quarantine；
/// 4. Interface：解绑 active bindings，丢弃 staged pending publications。
///
/// 调用方负责状态提交（失败 = `Failed`；优雅停止 = 随后 `Stopping → Stopped`）。
pub(crate) fn revoke_authority_and_unbind(id: ComponentId) {
    irq::revoke_owner(id);
    dma::revoke_owner(id);
    device::quarantine_owner(id);
    let mut ifs = interface::get_interfaces().lock();
    // active bindings：provider 解绑（consumer 立即不可 bind/refresh）。
    ifs.unbind_provider(id);
    // staged publish：pending 全丢弃，旧 provider 完全不受影响。
    ifs.discard_pending(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentState;
    use crate::component::interface::{self, InterfaceAbi, InterfaceError, InterfaceKind};
    use crate::component::registry;
    use crate::machine::{CompatStr, DeviceDescriptor, IoSpace};
    use crate::resource::{RequestContext, device, dma, irq};

    const ABI: InterfaceAbi = InterfaceAbi::from_raw(0xFA11_0001);

    extern "C" fn demo_impl(_ctx: *mut (), _input: u32) -> u32 {
        0
    }

    extern "C" fn demo_irq(_ctx: *mut ()) {}

    fn commit_device(device_index: usize, compatible: &[u8]) {
        use crate::machine::{self, CpuId, CpuInfo, MachineInfo, MemoryRegion};
        let mut devices = [DeviceDescriptor::empty(); 26];
        devices[device_index] = DeviceDescriptor {
            space: IoSpace::Mmio {
                base: 0x1000_0000 + device_index * 0x1000,
                size: 0x1000,
            },
            irq: Some(8),
            compatibles: [
                CompatStr::from_bytes(compatible),
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
            dev_count: device_index + 1,
            devices,
        });
    }

    /// 失败编排：资源回收 + 接口解绑 + 丢弃 pending + 提交 Failed + 设备 quarantine。
    #[test]
    fn fail_component_revokes_resources_and_unbinds_interfaces() {
        let _machine = crate::machine::test_support::GUARD.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        crate::component::image::init();
        interface::init();
        crate::resource::init();
        commit_device(24, b"fail,mmio");

        let image = crate::component::image::test_support::register_test_image(b"fail_demo", 0);
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(image).unwrap();
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
            .register(id, 24, 8, demo_irq, core::ptr::null_mut());
        let buffer = dma::alloc(id, 4096).expect("dma alloc");
        let mapping = dma::map(
            &ctx,
            device_id,
            buffer.ptr,
            buffer.len,
            dma::DmaDirection::ToDevice,
        )
        .expect("dma map");
        {
            let reg = registry::get_registry().lock();
            let mut ifs = interface::get_interfaces().lock();
            ifs.stage_publish(
                &reg,
                id,
                b"fail_demo_iface",
                InterfaceKind::Service,
                ABI,
                demo_impl as *const (),
                core::ptr::null_mut(),
            )
            .unwrap();
            ifs.commit_pending(&reg, id).unwrap();
        }
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
        assert!(!device::get_table().lock().owner(24).is_some());
        assert!(device::get_table().lock().is_quarantined(24));

        // quarantine 后普通认领 -EBUSY。
        let claimant = RequestContext {
            component: ComponentId::from_raw(999),
            task: None,
        };
        assert_eq!(
            device::claim(&claimant, device_id),
            Err(device::DeviceClaimError::DeviceBusy)
        );

        // 接口解绑。
        {
            let reg = registry::get_registry().lock();
            assert_eq!(
                interface::get_interfaces().lock().bind(
                    &reg,
                    b"fail_demo_iface",
                    InterfaceKind::Service,
                    ABI
                ),
                Err(InterfaceError::Unbound)
            );
        }
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Failed
        );

        device::get_table().lock().clear_quarantine();
    }
}
