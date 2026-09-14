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
//! - **拒绝来自 Failed 组件的新操作**：当前 Failed 组件再次调用 authority 入口时，
//!   已 revoke 的 handle 会因 generation 前进而 `Stale`；但「组件状态即拒绝」的
//!   统一 state check 尚未加进各 authority 操作（那需要入口统一收 `RequestContext`
//!   并查 registry），本增量不动。
//! - **停止组件的任务**：`stop_component_tasks` 需要 task-stop API（当前 Core 只有
//!   yield/exit，没有 Core 侧强制停止），本增量不动。
//! - **物理组件镜像回收**：Phase 1 保持 logical death / physical residency。

use crate::component::load::ComponentLoadError;
use crate::component::{ComponentId, interface, registry};
use crate::handle::{dma, irq, mmio};

/// 组件失败（逻辑死亡）的 Core 编排：`mark_failed` → 撤销 MMIO → 撤销 IRQ →
/// 撤销 DMA → 解绑它作为 provider 的所有接口并丢弃其 pending publications。
///
/// `reason` 记录失败原因；Registry 当前只存状态、不存 reason，参数保留为调用方
/// 语义 / 未来 trace seam。锁纪律：各操作各自取锁、互不嵌套，可安全调用。
pub fn fail_component(id: ComponentId, reason: ComponentLoadError) {
    let _ = reason;
    registry::get_registry().lock().mark_failed(id).ok();
    mmio::get_table().lock().revoke_owner(id);
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
                device_index: 0,
            },
        );
        let irq_handle = irq::get_table().lock().grant(id, Irq::new(8, 0));
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
    }
}
