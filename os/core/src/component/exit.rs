//! 组件停止（graceful stop）的 Core 编排。
//!
//! 与 [`super::failure`]（forced containment）对称：本模块是**优雅停止**的唯一
//! 汇合点——`Ready → Stopping → Stopped`，中途调用组件的销毁入口
//! `kcomp_instance_destroy(state)`。生产调用方是 monitor 的 `unload <name>` 命令。
//!
//! # 已实现的停止顺序（small option，Oracle 评审后定稿）
//!
//! ```text
//! 1. 拒绝门（任何提交之前；拒绝不改 Core 真相）
//!    a. 实例必须不拥有任何未退出的任务，否则拒绝（不 join、不等待）；
//!    b. begin_stop 还要求实例存在且处于 Ready（不存在 → NotFound；
//!       非 Ready → NotReady）；
//! 2. registry.begin_stop(id)      Ready → Stopping：提交"不再接受新 work"
//! 3. 调用组件销毁入口 kcomp_instance_destroy(state)
//!                                必需导出；Core-owned 隔离栈
//! 4. Core 兜底                     revoke authority + 解绑 provider interfaces
//!                                  （与 failure 路径共用同一序列，见 `failure.rs`）
//! 5. registry.finish_stop(id)     Stopping → Stopped（终态）
//! ```
//!
//! 为什么任务检查必须在 `begin_stop` **之前**：`may_run` 只允许
//! `Starting`/`Ready`，一旦提交 `Stopping`，该实例的任务就再也不可能被调度
//! 回来收尾——"先停后等任务"是自相矛盾的顺序。`yield` 只提交 `Runnable`
//! （不是 `Exited`），任务不会"自然退出"，所以本版不做等待；等任务清空的
//! drain variant 明确不在本增量（见下）。
//!
//! # 身份、栈与门禁
//!
//! - **执行上下文**：destroy 跑在 Core-owned 临时栈上（与 create 对称，
//!   见 [`containment::call_component_destroy`]）。
//! - **身份**：入口的 ambient identity = **被停止的实例**（`EscapeKind::Exit`），
//!   不是发起 stop 的 monitor / 其他组件，也不是 `load::current_component()`。
//!   这是本文件与 `containment` 协同保证的契约（host 测试锁定）。
//! - **入口可以释放 authority**：`release` / `revoke` 与已持有 handle 的操作
//!   不受生命周期门禁限制（见 `export.rs` 的门禁说明），销毁入口能在 `Stopping`
//!   状态下自行 `kcore_mmio_release` 等；入口返回后 Core 仍兜底撤销一切**剩余**
//!   authority（多撤不少撤；设备宁可进 quarantine 也不留悬空授权）。
//! - **失败路径刻意不调用本入口**（Linux 类比：崩溃的模块不值得信任）：
//!   [`super::failure::fail_component`] 直接 `mark_failed` + 同一兜底，不经过
//!   本文件。代价：组件侧的设备收尾（stop DMA / reset / mask IRQ）在失败路径上
//!   不会发生，Core 的 revoke + quarantine 是唯一兜底（见 docs/component-model.md
//!   §5.2）。
//!
//! # destroy 失败语义（契约 §8，已定稿）
//!
//! - `kcomp_instance_destroy` 返回非零 → 实例置 `Failed`（不是 `Stopped`），
//!   **保留内存**（state 存储不回收），Core containment 兜底；
//! - panic → 由 Destroy 边界容纳（`CallOutcome::Panicked`）→ 同上；
//! - **绝不自动重试析构**；`Failed` 是终态，tombstone 保留。
//!
//! # 明确 OUT OF SCOPE（本增量不做，留给人类决定）
//!
//! - **drain variant**（等任务自然退出再停）：需要 `may_run` 增加"停止中仍允许
//!   收尾"的语义 + 任务完成协议 / 超时，不是本版"先拒绝再停"能顺带做的；
//! - **task-stop / join API**：Core 仍无强制停止任务的能力；
//! - **销毁入口的阻塞 / 超时**：入口同步跑在调用者上下文中，无 watchdog；
//!   恶意/挂死的入口会挂住 stop（KernelNative 协作式信任，与 create 同）；
//! - **`UnexpectedExit` 终态**：意外退出统一由 `Failed` 覆盖；
//! - **退出期间的新 authority 门禁**：现有 export 门禁只拦 `Failed`；入口在
//!   `Stopping` 期间调用 `kcore_mmio_claim` / `kcore_dma_alloc` 等仍会成功，
//!   随后被第 4 步兜底撤销。硬拦需要在 export 门禁加入生命周期判定（未定稿）；
//! - **实例退役 / 段内存回收**：`Stopped` 记录保留（phase 1：逻辑死亡、物理驻留），
//!   registry 不删除记录、image 不 unload。

use crate::component::containment::{self, CallOutcome};
use crate::component::image;
use crate::component::load::ComponentLoadError;
use crate::component::registry::{self, RegistryError};
use crate::component::{ComponentId, failure};

/// 停止的拒绝 / 失败原因（`stop_component` 的返回错误）。
///
/// ABI 语义：当前只有 monitor `unload` 与 host 测试消费；errno 映射已定
/// （`errno.rs::From<ComponentStopError>`），未来若导出 `kcore_component_stop`
/// 直接复用，不需要重新定档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentStopError {
    /// 实例不存在（未声明；phase 1 无 unload）。errno 语义：`ENOENT`。
    NotFound,
    /// 实例不在 `Ready`：只有 `Ready` 有完整、已提交的初始化状态可停止。
    /// 重复 stop（`Stopping` / `Stopped`）与 `Failed` 实例都落到这里。
    /// errno 语义：`EINVAL`（状态机拒绝转换）。
    NotReady,
    /// 实例仍拥有未退出的任务，拒绝停止（小方案不做 join / 不等待）。
    /// errno 语义：`EBUSY`（in use）。
    OwnsLiveTasks,
    /// `kcomp_instance_destroy` 返回非零（实例 → `Failed` + 兜底，不重试）。
    /// errno 语义：`EIO`。
    DestroyFailed(i32),
    /// `kcomp_instance_destroy` panic，已由 Destroy 边界切回 Core（同上）。
    /// errno 语义：`EIO`。
    DestroyPanicked,
    /// 停止途中的状态机提交失败（只会由并发 stop/fail 触发；phase 1 单核不可达，
    /// 不静默）。errno 语义：`EIO`。
    StateRejected,
}

/// 优雅停止一个组件实例：small option 的完整编排（顺序见模块文档）。
///
/// 成功 = `Stopped`（记录保留）；失败 = 拒绝（真相不变）或销毁入口失败（`Failed`）。
/// `kcomp_instance_destroy` 是**必需导出**（loader 保证每个 image 都有）。
pub fn stop_component(id: ComponentId) -> Result<(), ComponentStopError> {
    // 步骤 1：拒绝门。必须在任何提交之前——拒绝不得改变 Core 真相。
    // 1a. 仍拥有未退出任务？只读扫现有任务表（不建第二账本）。
    //     与 1b 之间没有可运行窗口：phase 1 单核协作式，当前执行不在任何组件
    //     任务里；且 `begin_stop` 提交后 `kcore_task_create` 也不再放行该实例。
    if crate::task::get_task_table().lock().has_live_tasks(id) {
        return Err(ComponentStopError::OwnsLiveTasks);
    }
    // 1b. `Ready → Stopping` 是唯一合法起点；非 Ready / 不存在由规则表拒绝。
    if let Err(error) = registry::get_registry().lock().begin_stop(id) {
        return Err(match error {
            RegistryError::NotFound => ComponentStopError::NotFound,
            _ => ComponentStopError::NotReady,
        });
    }

    // 步骤 2：必需销毁入口。参数 = 实例在 create 时记录的 opaque state
    // （可为 NULL，无状态组件合法）。identity = 被停止实例（containment 的 Exit 边界）。
    let (image_id, instance_state) = {
        let reg = registry::get_registry().lock();
        let record = reg.get(id).expect("begin_stop 后实例必然存在");
        (record.image, record.instance_state)
    };
    let destroy = image::get_images()
        .lock()
        .get(image_id)
        .map(|image| image.destroy)
        .expect("实例的 image 必然常驻登记（pinned-until-reboot）");
    let outcome = containment::call_component_destroy(destroy, instance_state, id);

    // 步骤 3 的结果分类 + 步骤 4/5：兜底 → `Stopped`。
    complete_stop(id, outcome)
}

/// 销毁入口结果 → 终态提交（`stop_component` 的尾段）。
///
/// 独立成函数，让 host 测试能直接驱动非零 / panic 分类：fake 后端不做真实
/// 上下文切换，入口本体在 host 上不会被执行（QEMU gate 用 kcomp_smoke 的
/// 可观测行证明真实执行）。
fn complete_stop(id: ComponentId, outcome: CallOutcome) -> Result<(), ComponentStopError> {
    match outcome {
        CallOutcome::Returned(0) => {}
        CallOutcome::Returned(code) => {
            // 契约 §8：destroy 失败 → Failed + 保留内存 + Core 兜底；绝不重试。
            failure::fail_component(id, ComponentLoadError::DestroyFailed(code));
            return Err(ComponentStopError::DestroyFailed(code));
        }
        CallOutcome::Panicked => {
            // 契约 §8：destroy panic → 同上（不重试、不进入 Stopped）。
            failure::fail_component(id, ComponentLoadError::DestroyPanicked);
            return Err(ComponentStopError::DestroyPanicked);
        }
    }
    // 组件自行收尾之后，Core 仍然兜底收回剩余 authority / 解绑 provider。
    failure::revoke_authority_and_unbind(id);
    // 不变式：begin_stop 已提交 Stopping，本转换只可能被并发 stop/fail 拒绝
    // （phase 1 单核不可达）；失败不静默。
    if registry::get_registry().lock().finish_stop(id).is_err() {
        return Err(ComponentStopError::StateRejected);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentState;
    use crate::component::image::{self, ComponentImageId};
    use crate::handle::RequestContext;
    use crate::task::TaskState;

    /// 登记一份测试 image（同名复用），带指定的 destroy 入口。
    ///
    /// 调用方须已持有 memory GUARD（分配常驻 lease）。
    fn test_image(name: &[u8], destroy: usize) -> ComponentImageId {
        image::test_support::register_test_image(name, destroy)
    }

    /// 测试用实例：登记一份 image，声明实例并走到 `Ready`。
    fn ready_component(name: &[u8], destroy: usize) -> ComponentId {
        let image = test_image(name, destroy);
        let mut reg = registry::get_registry().lock();
        let id = reg.declare(image).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    /// 只声明（不 resolve）的实例，用于拒绝门测试。
    fn declared_component(name: &[u8]) -> ComponentId {
        let image = test_image(name, 0);
        registry::get_registry().lock().declare(image).unwrap()
    }

    fn setup() -> crate::memory::test_support::Guard<'static> {
        registry::init();
        image::init();
        crate::task::init();
        crate::handle::init();
        crate::component::interface::init();
        let guard = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        guard
    }

    extern "C" fn destroy_hook_ok(_state: *mut ()) -> i32 {
        0
    }

    #[test]
    fn stop_refuses_when_instance_owns_unfinished_task() {
        // Given：Ready 组件 + 一个属于它的 Created 任务。
        let _heap = setup();
        let id = ready_component(b"exit_live_task", destroy_hook_ok as *const () as usize);
        let task = crate::task::get_task_table()
            .lock()
            .create(id, 0x1000, core::ptr::null_mut())
            .unwrap();

        // When：请求停止。
        let result = stop_component(id);

        // Then：拒绝、真相不变（仍 Ready），任务不被改动（无 join / 无 kill）。
        assert_eq!(result, Err(ComponentStopError::OwnsLiveTasks));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Ready
        );
        assert_eq!(
            crate::task::get_task_table()
                .lock()
                .get(task)
                .unwrap()
                .state(),
            TaskState::Created
        );

        // 清理：移除任务（Drop 归还 kstack 区域）。
        let _ = crate::task::get_task_table().lock().remove(task);
    }

    #[test]
    fn stop_drives_ready_to_stopped_through_destroy_entry() {
        let _heap = setup();
        // Given：一个 Ready 实例（image 带 destroy 入口）。
        let id = ready_component(b"exit_destroy_ok", destroy_hook_ok as *const () as usize);

        // When：停止。
        // Then：销毁入口分支走通到 Stopped。fake 后端不做真实上下文切换，入口本体
        // 在 host 上不会执行——真实执行由 QEMU gate 的可观测行证明。
        let result = stop_component(id);
        assert_eq!(result, Ok(()));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Stopped
        );
    }

    #[test]
    fn destroy_identity_is_the_stopped_instance() {
        // Given：`call_component_destroy` 安装的 Exit 边界（fake 后端切不了上下文，
        // 用 containment 的测试边界复现同一 guard）。
        let _boundary = containment::test_boundary_lock();
        let stopped = ComponentId::from_raw(0xE217);

        containment::with_test_exit_boundary(stopped, || {
            // When：入口内解析 Core 调用身份。
            let ambient = RequestContext::ambient().expect("destroy identity");

            // Then：就是被停止的实例，不是 monitor / 其他组件，也没有任务身份；
            // publish 仍是 init 期操作（不是合法 principal）。
            assert_eq!(ambient.component, stopped);
            assert_eq!(ambient.task, None);
            assert!(RequestContext::ambient_init().is_none());
        });
    }

    #[test]
    fn double_stop_is_refused_and_keeps_stopped() {
        let _heap = setup();
        // Given：一个已经停止的实例。
        let id = ready_component(b"exit_double_stop", destroy_hook_ok as *const () as usize);
        assert_eq!(stop_component(id), Ok(()));

        // When / Then：二次停止被规则表拒绝，真相保持 Stopped。
        assert_eq!(stop_component(id), Err(ComponentStopError::NotReady));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Stopped
        );
    }

    #[test]
    fn stop_refuses_unknown_and_non_ready_instances() {
        let _heap = setup();

        // 未知 id：NotFound（任务扫描找不到任何归属，begin_stop 报 NotFound）。
        assert_eq!(
            stop_component(ComponentId::from_raw(0xDEAD)),
            Err(ComponentStopError::NotFound)
        );

        // Declared（未走完 init）：NotReady，且真相不变。
        let id = declared_component(b"exit_declared");
        assert_eq!(stop_component(id), Err(ComponentStopError::NotReady));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Declared
        );
    }

    #[test]
    fn destroy_non_zero_fails_component_and_reports_destroy_failed() {
        let _heap = setup();
        // Given：一个已进入 Stopping 的实例（stop_component 中入口返回后的状态）。
        let id = ready_component(b"exit_nonzero", destroy_hook_ok as *const () as usize);
        registry::get_registry().lock().begin_stop(id).unwrap();

        // When：销毁入口返回非零（host 直接驱动尾段；fake 后端不执行入口本体）。
        let result = complete_stop(id, CallOutcome::Returned(7));

        // Then：契约 §8 —— Failed + 兜底，不进入 Stopped，绝不自动重试。
        assert_eq!(result, Err(ComponentStopError::DestroyFailed(7)));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Failed
        );
    }

    #[test]
    fn destroy_panic_fails_component_and_reports_destroy_panicked() {
        let _heap = setup();
        // Given：一个已进入 Stopping 的实例。
        let id = ready_component(b"exit_panicked", destroy_hook_ok as *const () as usize);
        registry::get_registry().lock().begin_stop(id).unwrap();

        // When：Destroy 边界切回并报告 panic。
        let result = complete_stop(id, CallOutcome::Panicked);

        // Then：契约 §8 —— Failed + 兜底（同非零返回；不重试）。
        assert_eq!(result, Err(ComponentStopError::DestroyPanicked));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Failed
        );
    }

    #[test]
    fn stop_backstop_revokes_leftover_mmio_authority() {
        // Given：Ready 组件 + 一个它没来得及释放的 MMIO authority。
        // machine GUARD 与 failure.rs 的 quarantine 用例串行。
        let _machine = crate::machine::test_support::GUARD.lock();
        let _heap = setup();
        let id = ready_component(b"exit_leftover_mmio", destroy_hook_ok as *const () as usize);
        let handle = crate::handle::mmio::get_table().lock().grant(
            id,
            crate::handle::mmio::MmioRegion {
                base: 0x3000_0000,
                size: 0x1000,
                device_index: 23,
            },
        );

        // When：停止（销毁入口什么也没释放）。
        assert_eq!(stop_component(id), Ok(()));

        // Then：Core 兜底撤销了剩余 authority（与 failure 路径同一序列）。
        assert_eq!(
            crate::handle::mmio::get_table()
                .lock()
                .get(id, handle)
                .map(|_| ()),
            Err(crate::handle::HandleError::Stale)
        );
        assert!(crate::handle::mmio::get_table().lock().is_quarantined(23));

        // 清理：进程全局 quarantine 标记不能在用例间残留。
        crate::handle::mmio::get_table().lock().clear_quarantine();
    }

    /// 契约核心：停止**只影响被选中的实例**——共享同一 image 的另一个实例
    /// 保持 Ready，其 authority 不受影响。
    #[test]
    fn stop_affects_only_the_selected_instance() {
        // Given：两个共享同一 image 的 Ready 实例，各自持有一份 MMIO authority。
        let _machine = crate::machine::test_support::GUARD.lock();
        let _heap = setup();
        let image = test_image(b"exit_two_instances", destroy_hook_ok as *const () as usize);
        let (first, second) = {
            let mut reg = registry::get_registry().lock();
            let first = reg.declare(image).unwrap();
            reg.resolve(first).unwrap();
            reg.begin_start(first).unwrap();
            reg.finish_start(first).unwrap();
            let second = reg.declare(image).unwrap();
            reg.resolve(second).unwrap();
            reg.begin_start(second).unwrap();
            reg.finish_start(second).unwrap();
            (first, second)
        };
        assert_ne!(first, second, "两个实例身份不同");
        let first_handle = crate::handle::mmio::get_table().lock().grant(
            first,
            crate::handle::mmio::MmioRegion {
                base: 0x3100_0000,
                size: 0x1000,
                device_index: 30,
            },
        );
        let second_handle = crate::handle::mmio::get_table().lock().grant(
            second,
            crate::handle::mmio::MmioRegion {
                base: 0x3200_0000,
                size: 0x1000,
                device_index: 31,
            },
        );

        // When：只停止第一个实例。
        assert_eq!(stop_component(first), Ok(()));

        // Then：第一个 Stopped + authority 撤销；第二个仍 Ready + handle 仍可用。
        let reg = registry::get_registry().lock();
        assert_eq!(reg.get(first).unwrap().state, ComponentState::Stopped);
        assert_eq!(reg.get(second).unwrap().state, ComponentState::Ready);
        drop(reg);
        assert_eq!(
            crate::handle::mmio::get_table()
                .lock()
                .get(first, first_handle)
                .map(|_| ()),
            Err(crate::handle::HandleError::Stale)
        );
        assert!(
            crate::handle::mmio::get_table()
                .lock()
                .get(second, second_handle)
                .is_ok(),
            "未选中实例的 authority 必须原样"
        );

        // 清理：释放未选中实例的 authority，并清掉进程全局 quarantine 标记。
        crate::handle::mmio::get_table()
            .lock()
            .release(second, second_handle)
            .unwrap();
        crate::handle::mmio::get_table().lock().clear_quarantine();
    }
}
