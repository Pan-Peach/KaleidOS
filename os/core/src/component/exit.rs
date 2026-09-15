//! 组件退出（graceful stop）的 Core 编排。
//!
//! 与 [`super::failure`]（forced containment）对称：本模块是**优雅停止**的唯一
//! 汇合点——`Ready → Stopping → Stopped`，中途调用组件的可选退出钩子
//! `kcomp_exit`（Linux `module_exit` 类比）。生产调用方是 monitor 的
//! `unload <name>` 命令。
//!
//! # 已实现的停止顺序（small option，Oracle 评审后定稿）
//!
//! ```text
//! 1. 拒绝门（任何提交之前；拒绝不改 Core 真相）
//!    a. 实例必须不拥有任何未退出的任务，否则拒绝（不 join、不等待）；
//!    b. begin_stop 还要求实例存在且处于 Ready（不存在 → NotFound；
//!       非 Ready → NotReady）；
//! 2. registry.begin_stop(id)      Ready → Stopping：提交"不再接受新 work"
//! 3. 调用组件退出钩子 kcomp_exit    可选；导出该符号才调用（Core-owned 隔离栈）
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
//! - **执行上下文**：`kcomp_exit` 跑在 Core-owned 临时栈上（与 `kcomp_init`
//!   对称，见 [`containment::call_component_exit`]）。
//! - **身份**：钩子的 ambient identity = **被停止的实例**（`EscapeKind::Exit`），
//!   不是发起 stop 的 monitor / 其他组件，也不是 `load::current_component()`。
//!   这是本文件与 `containment` 协同保证的契约（host 测试锁定）。
//! - **钩子可以释放 authority**：`release` / `revoke` 与已持有 handle 的操作
//!   不受生命周期门禁限制（见 `export.rs` 的门禁说明），退出钩子能在 `Stopping`
//!   状态下自行 `kcore_mmio_release` 等；钩子返回后 Core 仍兜底撤销一切**剩余**
//!   authority（多撤不少撤；设备宁可进 quarantine 也不留悬空授权）。
//! - **失败路径刻意不调用本钩子**（Linux 类比：崩溃的模块不值得信任）：
//!   [`super::failure::fail_component`] 直接 `mark_failed` + 同一兜底，不经过
//!   本文件。代价：组件侧的设备收尾（stop DMA / reset / mask IRQ）在失败路径上
//!   不会发生，Core 的 revoke + quarantine 是唯一兜底（见 docs/component-model.md
//!   §5.2）。
//!
//! # 暂定默认（**待人类定稿**）
//!
//! 1. `kcomp_exit` 返回非零 → 与 init 失败同路：`Failed` + 兜底，返回
//!    [`ComponentStopError::ExitFailed`]（镜像 `InitFailed`）；
//! 2. `kcomp_exit` panic → 由 Exit 边界容纳（`CallOutcome::Panicked`）→ 同上，
//!    返回 [`ComponentStopError::ExitPanicked`]（镜像 `InitPanicked`）。
//!
//! 两者都是"镜像 init 语义"的暂定选择，未定稿；见 `docs/component-model.md`
//! §5.2 的开放问题清单。
//!
//! # 明确 OUT OF SCOPE（本增量不做，留给人类决定）
//!
//! - **drain variant**（等任务自然退出再停）：需要 `may_run` 增加"停止中仍允许
//!   收尾"的语义 + 任务完成协议 / 超时，不是本版"先拒绝再停"能顺带做的；
//! - **task-stop / join API**：Core 仍无强制停止任务的能力；
//! - **退出钩子的阻塞 / 超时**：钩子同步跑在调用者上下文中，无 watchdog；
//!   恶意/挂死的钩子会挂住 stop（KernelNative 协作式信任，与 `kcomp_init` 同）；
//! - **`UnexpectedExit` 终态**：意外退出统一由 `Failed` 覆盖；
//! - **退出期间的新 authority 门禁**：现有 export 门禁只拦 `Failed`；钩子在
//!   `Stopping` 期间调用 `kcore_mmio_claim` / `kcore_dma_alloc` 等仍会成功，
//!   随后被第 4 步兜底撤销。硬拦需要在 export 门禁加入生命周期判定（未定稿）；
//! - **实例退役 / 段内存回收**：`Stopped` 记录保留（phase 1：逻辑死亡、物理驻留），
//!   `Registry::unload`（删记录）不由本路径调用。

use crate::component::containment::{self, CallOutcome};
use crate::component::load::ComponentLoadError;
use crate::component::registry::{self, RegistryError};
use crate::component::{ComponentId, failure};

/// 优雅停止的拒绝 / 失败原因（`stop_component` 的返回错误）。
///
/// ABI 语义：当前只有 monitor `unload` 与 host 测试消费；errno 映射已定
/// （`errno.rs::From<ComponentStopError>`），未来若导出 `kcore_component_unload`
/// 直接复用，不需要重新定档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentStopError {
    /// 实例不存在（未声明或已卸载）。errno 语义：`ENOENT`。
    NotFound,
    /// 实例不在 `Ready`：只有 `Ready` 有完整、已提交的初始化状态可停止。
    /// 重复 stop（`Stopping` / `Stopped`）与 `Failed` 实例都落到这里。
    /// errno 语义：`EINVAL`（状态机拒绝转换）。
    NotReady,
    /// 实例仍拥有未退出的任务，拒绝停止（小方案不做 join / 不等待）。
    /// errno 语义：`EBUSY`（in use）。
    OwnsLiveTasks,
    /// `kcomp_exit` 返回非零（暂定：与 init 失败同路 → `Failed` + 兜底）。
    /// errno 语义：`EIO`。
    ExitFailed(i32),
    /// `kcomp_exit` panic，已由 Exit 边界切回 Core（暂定：与 init panic 同路）。
    /// errno 语义：`EIO`。
    ExitPanicked,
    /// 停止途中的状态机提交失败（只会由并发 stop/fail 触发；phase 1 单核不可达，
    /// 不静默）。errno 语义：`EIO`。
    StateRejected,
}

/// 优雅停止一个组件实例：small option 的完整编排（顺序见模块文档）。
///
/// 成功 = `Stopped`（记录保留）；失败 = 拒绝（真相不变）或钩子失败（`Failed`）。
/// `kcomp_exit` 是**可选**符号：`ComponentRecord.exit == None` 时跳过钩子，
/// 组件仍然干净地走到 `Stopped`。
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

    // 步骤 2：可选退出钩子。identity = 被停止实例（containment 的 Exit 边界）。
    let exit = registry::get_registry()
        .lock()
        .get(id)
        .and_then(|record| record.exit);
    let outcome = match exit {
        Some(entry) => containment::call_component_exit(entry, id),
        // 组件没有导出 kcomp_exit：跳过钩子，仍然干净停止。
        None => CallOutcome::Returned(0),
    };

    // 步骤 3 的结果分类 + 步骤 4/5：兜底 → `Stopped`。
    complete_stop(id, outcome)
}

/// 退出钩子结果 → 终态提交（`stop_component` 的尾段）。
///
/// 独立成函数，让 host 测试能直接驱动非零 / panic 分类：fake 后端不做真实
/// 上下文切换，钩子本体在 host 上不会被执行（QEMU gate 用 kcomp_smoke 的
/// 可观测行证明真实执行）。
fn complete_stop(id: ComponentId, outcome: CallOutcome) -> Result<(), ComponentStopError> {
    match outcome {
        CallOutcome::Returned(0) => {}
        CallOutcome::Returned(code) => {
            // 暂定（待人类定稿）：镜像 `InitFailed` —— Failed + 兜底。
            failure::fail_component(id, ComponentLoadError::ExitFailed(code));
            return Err(ComponentStopError::ExitFailed(code));
        }
        CallOutcome::Panicked => {
            // 暂定（待人类定稿）：镜像 `InitPanicked` —— Failed + 兜底。
            failure::fail_component(id, ComponentLoadError::ExitPanicked);
            return Err(ComponentStopError::ExitPanicked);
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
    use crate::handle::RequestContext;
    use crate::task::TaskState;

    /// 测试用组件：走完加载链到 `Ready`，并可选挂一个退出钩子。
    ///
    /// 全局表（registry / task / handle / interface）是进程级 `Once`；
    /// memory GUARD 串行化会分配 kstack / 隔离栈的用例。
    fn ready_component(name: &[u8], exit: Option<usize>) -> ComponentId {
        let mut reg = registry::get_registry().lock();
        let id = reg.declare(name, 0x1000, 0x2000, None).unwrap();
        reg.record_exit(id, exit).unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    fn setup() -> crate::memory::test_support::Guard<'static> {
        registry::init();
        crate::task::init();
        crate::handle::init();
        crate::component::interface::init();
        let guard = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        guard
    }

    extern "C" fn exit_hook_ok() -> i32 {
        0
    }

    #[test]
    fn stop_refuses_when_instance_owns_unfinished_task() {
        // Given：Ready 组件 + 一个属于它的 Created 任务。
        let _heap = setup();
        let id = ready_component(b"exit_live_task", None);
        let task = crate::task::get_task_table()
            .lock()
            .create(id, 0x1000)
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
    fn stop_drives_ready_to_stopped_and_skips_absent_hook() {
        let _heap = setup();
        // Given：没有导出 kcomp_exit 的组件（exit == None）。
        let id = ready_component(b"exit_no_hook", None);
        assert_eq!(registry::get_registry().lock().get(id).unwrap().exit, None);

        // When：停止。
        let result = stop_component(id);

        // Then：跳过钩子，但兜底与状态提交照常走完（Ready → Stopping → Stopped）。
        assert_eq!(result, Ok(()));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Stopped
        );
    }

    #[test]
    fn stop_takes_the_exit_hook_path_when_symbol_present() {
        let _heap = setup();
        // Given：导出 kcomp_exit 的组件。
        let id = ready_component(
            b"exit_hook_present",
            Some(exit_hook_ok as *const () as usize),
        );

        // When：停止。
        let result = stop_component(id);

        // Then：钩子分支走通到 Stopped。fake 后端不做真实上下文切换，钩子本体
        // 在 host 上不会执行——真实执行由 QEMU gate 的可观测行证明。
        assert_eq!(result, Ok(()));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Stopped
        );
    }

    #[test]
    fn exit_hook_identity_is_the_stopped_instance() {
        // Given：`call_component_exit` 安装的 Exit 边界（fake 后端切不了上下文，
        // 用 containment 的测试边界复现同一 guard）。
        let _boundary = containment::test_boundary_lock();
        let stopped = ComponentId::from_raw(0xE217);

        containment::with_test_exit_boundary(stopped, || {
            // When：钩子内解析 Core 调用身份。
            let ambient = RequestContext::ambient().expect("exit identity");

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
        let id = ready_component(b"exit_double_stop", None);
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
        let id = registry::get_registry()
            .lock()
            .declare(b"exit_declared", 1, 2, None)
            .unwrap();
        assert_eq!(stop_component(id), Err(ComponentStopError::NotReady));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Declared
        );
    }

    #[test]
    fn exit_non_zero_fails_component_and_reports_exit_failed() {
        let _heap = setup();
        // Given：一个已进入 Stopping 的实例（stop_component 中钩子返回后的状态）。
        let id = ready_component(b"exit_nonzero", None);
        registry::get_registry().lock().begin_stop(id).unwrap();

        // When：钩子返回非零（host 直接驱动尾段；fake 后端不执行钩子本体）。
        let result = complete_stop(id, CallOutcome::Returned(7));

        // Then：暂定默认 —— 镜像 InitFailed：Failed + 兜底，不进入 Stopped。
        assert_eq!(result, Err(ComponentStopError::ExitFailed(7)));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Failed
        );
    }

    #[test]
    fn exit_panic_fails_component_and_reports_exit_panicked() {
        let _heap = setup();
        // Given：一个已进入 Stopping 的实例。
        let id = ready_component(b"exit_panicked", None);
        registry::get_registry().lock().begin_stop(id).unwrap();

        // When：Exit 边界切回并报告 panic。
        let result = complete_stop(id, CallOutcome::Panicked);

        // Then：暂定默认 —— 镜像 InitPanicked：Failed + 兜底。
        assert_eq!(result, Err(ComponentStopError::ExitPanicked));
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
        let id = ready_component(b"exit_leftover_mmio", None);
        let handle = crate::handle::mmio::get_table().lock().grant(
            id,
            crate::handle::mmio::MmioRegion {
                base: 0x3000_0000,
                size: 0x1000,
                device_index: 23,
            },
        );

        // When：停止（组件没有钩子，什么也没释放）。
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
}
