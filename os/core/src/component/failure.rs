//! 组件失败的 Core 编排：标记 Failed 并回收它持有的 authority / 解绑它提供的接口。
//!
//! 落地 `docs/component-model.md` §4.9 的 `fail_component`（最小版）与
//! `docs/driver-model.md` §7 的撤销不变式：**组件失败 = 逻辑死亡、物理驻留**。
//! 顺序固定：先提交状态真相（Failed），再逐表 revoke 它持有的 authority，
//! 最后清掉它作为 provider 的全部 binding（含未提交的 pending publications）。
//! 资源表 revoke 只前进 generation，不回收物理驻留（phase 1 无隔离，回收留给
//! 未来 ExecutionDomain）。
//!
//! # 明确 DEFERRED（本增量不做）
//!
//! - **强制停止失败组件的任务**：Core 已从 runnable 候选与 commit 路径剔除
//!   `Failed` 组件拥有的任务（`sched.rs`），但不会**强制停止**正在跑的任务——
//!   那需要 task-stop API（当前只有 yield/exit），本增量不做；任务在下次
//!   yield/exit 时自然退出。
//! - **Stopping / Stopped 与 `kcomp_exit`**：本增量只把 `Failed` 接入
//!   acquiring / work 门禁（`component::is_failed` / `component::may_run`，入口见
//!   `export.rs`）；`kcomp_exit` 只被 loader 可选解析为 seam（从不调用），优雅
//!   quiesce / drain / 实例退役留给后续增量。
//!   TODO(unexpected-exit): 本文件是失败实例状态提交的汇合点——未来"独立
//!   abort/exit 通知"（区分普通失败与组件主动退出）会从这里分流。
//! - **物理组件镜像回收**：Phase 1 保持 logical death / physical residency。

use crate::component::load::ComponentLoadError;
use crate::component::{ComponentId, interface, registry};
use crate::handle::{dma, irq, mmio};

/// 组件失败（逻辑死亡）的 Core 编排：`mark_failed` → quarantine + 撤销 MMIO →
/// 撤销 IRQ → 撤销 DMA → 解绑它作为 provider 的所有接口并丢弃其 pending publications。
///
/// **设备 quarantine**：revoke MMIO authority 不等于设备可被下一个驱动安全复用
/// （设备可能仍被硬件引用 / 未静默）。因此撤销前先把失败组件占用的每个
/// `device_index` 标进 Core 的失败 quarantine——之后普通认领返回 `-EBUSY`，直到
/// reboot（phase 1 不建 reset/recovery 框架）。组件**优雅、协作式 quiesce** 后的
/// `release` 不进入 quarantine，设备仍可复用。
///
/// `reason` 记录失败原因；Registry 当前只存状态、不存 reason，参数保留为调用方
/// 语义 / 未来 trace seam。锁纪律：各操作各自取锁、互不嵌套，可安全调用。
pub fn fail_component(id: ComponentId, reason: ComponentLoadError) {
    let _ = reason;
    registry::get_registry().lock().mark_failed(id).ok();
    mmio::get_table().lock().quarantine_owner(id);
    irq::get_table().lock().revoke_owner(id);
    // DMA authority：failed 组件的 backing lease 进 QUARANTINE（不 free）。
    // （`dma::revoke_owner` 早已存在，此前未接在失败路径上。）
    dma::get_table().lock().revoke_owner(id);
    let mut ifs = interface::get_interfaces().lock();
    // active bindings：provider 解绑（consumer 立即不可 bind/refresh）。
    ifs.unbind_provider(id);
    // staged publish：init 失败/panic 时 pending 全丢弃，旧 provider 完全不受影响。
    ifs.discard_pending(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentState;
    use crate::component::interface::{self, InterfaceAbi, InterfaceError, InterfaceKind};
    use crate::component::registry;
    use crate::handle::HandleError;
    use crate::handle::RequestContext;
    use crate::handle::dma::{self, DmaDirection};
    use crate::handle::irq::{self, Irq};
    use crate::handle::mmio::{self, MmioRegion};

    const ABI: InterfaceAbi = InterfaceAbi::from_raw(0xFA11_0001);

    extern "C" fn demo_impl(_ctx: *mut (), _input: u32) -> u32 {
        0
    }

    /// 失败编排回收 authority + 解绑接口 + 丢弃 pending + 提交 Failed 状态。
    ///
    /// Given：全局表里一个 Starting 组件，持有 MMIO/IRQ/DMA authority 且作为接口 provider。
    /// When：先 finish_start（Ready）、再调用 `fail_component`。
    /// Then：三个 handle 变 Stale、接口不再可 bind、registry 状态为 Failed。
    #[test]
    fn fail_component_revokes_authority_and_unbinds_interfaces() {
        // 全局表是进程级 `Once`；claim 类测试走 machine GUARD，堆类测试走 memory GUARD。
        let _guard = crate::machine::test_support::GUARD.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        registry::init();
        interface::init();
        crate::handle::init();

        // Given：提交一份包含 device 24 的机器信息（之后用它验证 quarantine 认领）。
        // device_index 24 是本用例专用，避开其它测试的索引。
        {
            use crate::machine::{
                self, CompatStr, CpuId, CpuInfo, DeviceDescriptor, IoSpace, MachineInfo,
                MemoryRegion,
            };
            let mut devices = [DeviceDescriptor::empty(); 26];
            devices[24] = DeviceDescriptor {
                space: IoSpace::Mmio {
                    base: 0x1000_0000,
                    size: 0x1000,
                },
                irq: Some(8),
                compatibles: [
                    CompatStr::from_bytes(b"fail,mmio"),
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
                dev_count: 25,
                devices,
            });
        }

        // Given：Starting 组件 + 三个 authority + 一个已提交接口。
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg.declare(b"fail_demo", 1, 2, None).unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };
        let mmio_handle = mmio::get_table().lock().grant(
            id,
            MmioRegion {
                base: 0x1000_0000,
                size: 0x1000,
                device_index: 24,
            },
        );
        let irq_handle = irq::get_table().lock().grant(id, Irq::new(8, 24));
        // DMA authority：设备身份从 caller 已持有的 MmioHandle 推导。
        let dma_ctx = RequestContext {
            component: id,
            task: None,
        };
        let dma_handle =
            dma::alloc(&dma_ctx, mmio_handle, 4096, DmaDirection::ToDevice).expect("dma alloc");
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

        // When：Core 编排组件失败。
        fail_component(id, ComponentLoadError::InitFailed(1));

        // Then：三种 authority 都被撤销（generation 前进 → Stale）。
        assert_eq!(
            mmio::get_table().lock().get(id, mmio_handle).map(|_| ()),
            Err(HandleError::Stale),
            "MMIO handle 必须失效"
        );
        assert_eq!(
            irq::get_table().lock().get(id, irq_handle).map(|_| ()),
            Err(HandleError::Stale),
            "IRQ handle 必须失效"
        );
        assert_eq!(
            dma::get_table().lock().get(id, dma_handle).map(|_| ()),
            Err(HandleError::Stale),
            "DMA handle 必须失效"
        );

        // Then：接口解绑，consumer 不再能 bind。
        {
            let reg = registry::get_registry().lock();
            assert_eq!(
                interface::get_interfaces().lock().bind(
                    &reg,
                    b"fail_demo_iface",
                    InterfaceKind::Service,
                    ABI
                ),
                Err(InterfaceError::Unbound),
                "provider 失败后 binding 必须不可用"
            );
        }

        // Then：registry 状态提交为 Failed。
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Failed
        );

        // Then：失败设备被 quarantine——revoke 后既无 live claim，普通认领也返回
        // Busy（直到 reboot）。优雅 release 才会重新可认领。
        assert!(mmio::get_table().lock().is_quarantined(24));
        assert!(!mmio::get_table().lock().holds_device(24));
        let claimant = ComponentId::from_raw(999);
        let claim_ctx = RequestContext {
            component: claimant,
            task: None,
        };
        assert_eq!(
            mmio::claim_device(&claim_ctx, crate::machine::DeviceId::from_raw(24)),
            Err(mmio::MmioClaimError::DeviceBusy),
            "失败设备 quarantine 后普通认领必须 -EBUSY"
        );

        // 清理：进程全局 quarantine 标记不能在用例间残留。
        mmio::get_table().lock().clear_quarantine();
    }
}
