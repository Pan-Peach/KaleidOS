//! 组件停止（graceful stop）的 Core 编排。
//!
//! 与 [`super::failure`]（forced containment）对称：本模块是**优雅停止**的唯一
//! 汇合点——`Ready → Stopping → Stopped`，中途调用组件的销毁入口
//! `kcomp_instance_destroy(state)`。生产调用方是 monitor 与 `kcore_component_stop`。
//!
//! # 停止顺序（拒绝门在任何提交之前；拒绝不改 Core 真相）
//!
//! ```text
//! 1. 同一准入事务：Ready、无未退出任务 / 在途执行 / Native Direct 发布
//!    （不存在 → NotFound；非 Ready → NotReady）；
//! 2. registry.begin_stop(id)      Ready → Stopping：提交"不再接受新 work"
//! 3. 调用 kcomp_instance_destroy(state)  必需导出；按执行域选 Core 栈 / 私有 AS
//! 4. Core 兜底                     revoke authority + 永久失效 provider
//!                                  endpoints（与 failure 路径共用，见 `failure.rs`）
//! 5. registry.finish_stop(id)     Stopping → Stopped（终态）
//! ```
//!
//! 为什么任务检查必须在 `begin_stop` **之前**：`may_run` 只允许 `Starting`/`Ready`，
//! 一旦提交 `Stopping`，该实例的任务就再也不可能被调度回来收尾——"先停后等任务"
//! 是自相矛盾的顺序。`yield` 只提交 `Runnable`（不是 `Exited`），任务不会"自然
//! 退出"，所以本版不做等待，也不提供 drain variant / task-stop / join。
//!
//! # 身份、栈与门禁
//!
//! - **执行上下文**：KernelNative 的 destroy 跑在 Core-owned 临时栈上（与 create
//!   对称，见 [`containment::call_component_destroy`]）；Isolated 的 destroy 跑在
//!   该实例的私有 AS 内经 跨 AS trampoline（`isolated_lifecycle::destroy`），两者
//!   按 `ComponentRecord::execution_domain` 分派，**绝不静默互换**。
//! - **身份**：入口的 ambient identity = **被停止的实例**（`EscapeKind::Exit`），
//!   不是发起 stop 的 monitor / 其他组件，也不是 `load::current_component()`。
//!   这是本文件与 `containment` 协同保证的契约（host 测试锁定）。
//! - **入口可以释放 authority**：`release` / `revoke` 与已持有 handle 的操作
//!   不受生命周期门禁限制（见 `export.rs` 的门禁说明），销毁入口能在 `Stopping`
//!   状态下自行释放资源；入口返回后 Core 仍兜底撤销一切**剩余** authority（多撤
//!   不少撤；设备宁可进 quarantine 也不留悬空授权）。反向地，入口在 `Stopping`
//!   期间调用 `kcore_mmio_claim` / `kcore_dma_alloc` 等仍会成功，随后被第 4 步
//!   兜底撤销——现有 export 门禁只拦 `Failed`。
//! - **失败路径刻意不调用本入口**（Linux 类比：崩溃的模块不值得信任）：
//!   [`super::failure::fail_component`] 直接 `mark_failed` + 同一兜底，不经过
//!   本文件。代价：组件侧的设备收尾（stop DMA / reset / mask IRQ）在失败路径上
//!   不会发生，Core 的 revoke + quarantine 是唯一兜底（见
//!   `docs/architecture/component-model.md` §5.2）。
//! - destroy 入口同步跑在调用者上下文中，无 watchdog；挂死的入口会挂住 stop
//!   （KernelNative 协作式信任，与 create 同）。
//!
//! # destroy 失败语义
//!
//! - `kcomp_instance_destroy` 返回非零 → 实例置 `Failed`（不是 `Stopped`），
//!   **保留内存**（state 存储不回收），Core containment 兜底；
//! - panic → 由 Destroy 边界容纳（`CallOutcome::Panicked`）→ 同上；
//! - **绝不自动重试析构**；`Failed` 是终态，tombstone 保留。`Stopped` 记录同样
//!   保留（逻辑死亡、物理驻留），registry 不删除记录、image 不 unload。

use crate::component::containment::{self, CallOutcome};
use crate::component::endpoint::ExecutionDomain;
use crate::component::isolated_lifecycle;
use crate::component::load::ComponentLoadError;
use crate::component::registry::{self, RegistryError};
use crate::component::{ComponentId, failure};

/// 停止的拒绝 / 失败原因（`stop_component` 的返回错误）。
///
/// ABI 语义：当前只有 monitor `unload` 与 host 测试消费；errno 映射已定
/// （`errno.rs::From<ComponentStopError>`），若导出 `kcore_component_stop`
/// 直接复用，不需要重新定档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentStopError {
    /// 实例不存在（未声明；无 unload）。errno 语义：`ENOENT`。
    NotFound,
    /// 实例不在 `Ready`：只有 `Ready` 有完整、已提交的初始化状态可停止。
    /// 重复 stop（`Stopping` / `Stopped`）与 `Failed` 实例都落到这里。
    /// errno 语义：`EINVAL`（状态机拒绝转换）。
    NotReady,
    /// 实例仍拥有未退出的任务，拒绝停止（小方案不做 join / 不等待）。
    /// errno 语义：`EBUSY`（in use）。
    OwnsLiveTasks,
    /// Gate / policy / IRQ 尚未返回；拒绝且保持 Ready。
    ActiveExecutions,
    /// Native 已发布 Direct 表，无 release 协议，不能销毁可能外借的 ctx。
    DirectExports,
    /// `kcomp_instance_destroy` 返回非零（实例 → `Failed` + 兜底，不重试）。
    /// errno 语义：`EIO`。
    DestroyFailed(i32),
    /// `kcomp_instance_destroy` panic，已由 Destroy 边界切回 Core（同上）。
    /// errno 语义：`EIO`。
    DestroyPanicked,
    /// 停止途中的状态机提交失败（只会由并发 stop/fail 触发；单核不可达，
    /// 不静默）。errno 语义：`EIO`。
    StateRejected,
}

/// 优雅停止一个组件实例：small option 的完整编排（顺序见模块文档）。
///
/// 成功 = `Stopped`（记录保留）；失败 = 拒绝（真相不变）或销毁入口失败（`Failed`）。
/// `kcomp_instance_destroy` 是**必需导出**（loader 保证每个 image 都有）。
pub fn stop_component(id: ComponentId) -> Result<(), ComponentStopError> {
    // 步骤 1：拒绝门。必须在任何提交之前——拒绝不得改变 Core 真相。
    // Stop checks publication, execution and task admission under registry → endpoint/task locks. No remote
    // create can slip between checking live work and committing Stopping.
    {
        let _irq = crate::irq::IrqSaveGuard::new();
        let mut registry = registry::get_registry().lock();
        let record = registry.get(id).ok_or(ComponentStopError::NotFound)?;
        if record.state != crate::component::ComponentState::Ready {
            return Err(ComponentStopError::NotReady);
        }
        if record.execution_domain == ExecutionDomain::KernelNative
            && crate::component::endpoint::get_endpoints()
                .lock()
                .has_direct_exports(id)
        {
            return Err(ComponentStopError::DirectExports);
        }
        let table = crate::task::get_task_table().lock();
        if table.has_live_tasks(id) {
            return Err(ComponentStopError::OwnsLiveTasks);
        }
        if let Err(error) = registry.begin_stop(id) {
            return Err(match error {
                RegistryError::NotFound => ComponentStopError::NotFound,
                RegistryError::Busy => ComponentStopError::ActiveExecutions,
                _ => ComponentStopError::NotReady,
            });
        }
    }

    // 步骤 2：必需销毁入口。参数 = 实例在 create 时记录的 opaque state
    // （可为 NULL，无状态组件合法）。身份 = 被停止实例：KernelNative 走
    // containment 的 Exit 边界；Isolated 由 跨 AS trampoline 进入前的 CURRENT + AS owner 承载。
    // **按执行域分派**：KernelNative 在 Core 拥有的共享 AS 栈上调用；Isolated 在
    // 该实例的私有 AS 内经 跨 AS trampoline 调用（两者都绝不静默互换）。
    let (destroy, instance_state, domain) = {
        let reg = registry::get_registry().lock();
        let record = reg.get(id).expect("begin_stop 后组件必然存在");
        (
            record.loaded.destroy,
            record.instance_state,
            record.execution_domain,
        )
    };
    let outcome = match domain {
        ExecutionDomain::KernelNative => {
            containment::call_component_destroy(destroy, instance_state, id)
        }
        ExecutionDomain::IsolatedNative => isolated_lifecycle::destroy(id, destroy, instance_state),
        // Sandbox 执行器未实现（U-mode + 私有 AS + ecall）。
        ExecutionDomain::SandboxedNative => todo!("Sandbox 执行器未实现"),
    };

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
        CallOutcome::NoStack => {
            // 边界栈分配失败（Core 侧 `-ENOMEM`）：钩子从未执行，等价 destroy 失败。
            let code = containment::STACK_ALLOCATION_FAILED;
            failure::fail_component(id, ComponentLoadError::DestroyFailed(code));
            return Err(ComponentStopError::DestroyFailed(code));
        }
        CallOutcome::Panicked => {
            // 契约 §8：destroy panic → 同上（不重试、不进入 Stopped）。
            failure::fail_component(id, ComponentLoadError::DestroyPanicked);
            return Err(ComponentStopError::DestroyPanicked);
        }
    }
    // 组件自行收尾之后，Core 仍然兜底收回剩余 authority / 解绑 provider /
    // 使 provider endpoints 永久失效。
    failure::revoke_authority_and_unbind(id);
    // 不变式：begin_stop 已提交 Stopping，本转换只可能被并发 stop/fail 拒绝
    // （单核不可达）；失败不静默。
    if registry::get_registry().lock().finish_stop(id).is_err() {
        return Err(ComponentStopError::StateRejected);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ComponentState;
    use crate::component::endpoint::ExecutionDomain;
    use crate::resource::RequestContext;
    use crate::task::TaskState;

    /// 一份伪造 loaded image（无 backing），带指定的 destroy 入口。
    fn test_loaded(destroy: usize) -> crate::component::loader::LoadedComponent {
        crate::component::registry::test_support::test_loaded(destroy, None)
    }

    /// 测试用组件：声明并走到 `Ready`。
    fn ready_component(name: &[u8], destroy: usize) -> ComponentId {
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(name, test_loaded(destroy), ExecutionDomain::KernelNative)
            .unwrap();
        reg.resolve(id).unwrap();
        reg.begin_start(id).unwrap();
        reg.finish_start(id).unwrap();
        id
    }

    /// 只声明（不 resolve）的组件，用于拒绝门测试。
    fn declared_component(name: &[u8]) -> ComponentId {
        registry::get_registry()
            .lock()
            .declare(name, test_loaded(0), ExecutionDomain::KernelNative)
            .unwrap()
    }

    fn setup() -> crate::memory::test_support::Guard<'static> {
        registry::init();
        crate::task::init();
        crate::resource::init();
        crate::component::endpoint::init();
        let guard = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        guard
    }

    /// 安装一份含指定设备（MMIO + 一条绑定中断资源）的机器 fixture，并按尺寸
    /// 重建资源表。
    fn commit_devices(devices: &[(usize, &str)]) {
        use crate::machine::{
            CpuInfo, DeviceDescriptor, HardwareCpuId, InterruptResource, InterruptSpecifier,
            IoSpace, MemoryRegion,
        };
        let count = devices
            .iter()
            .map(|(index, _)| index + 1)
            .max()
            .unwrap_or(0);
        let mut table = alloc::vec![DeviceDescriptor::empty(); count];
        for (index, compatible) in devices {
            table[*index] = DeviceDescriptor {
                spaces: alloc::vec![IoSpace::Mmio {
                    base: 0x1000_0000 + *index * 0x1000,
                    size: 0x1000,
                }]
                .into_boxed_slice(),
                interrupts: alloc::vec![InterruptResource {
                    specifier: InterruptSpecifier::Isa { line: 8 },
                    line: Some(8),
                }]
                .into_boxed_slice(),
                compatibles: alloc::vec![alloc::boxed::Box::<str>::from(*compatible)]
                    .into_boxed_slice(),
            };
        }
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
            table,
        );
        crate::machine::test_support::install(info);
        crate::resource::test_support::reinstall();
    }

    extern "C" fn destroy_hook_ok(_state: *mut ()) -> i32 {
        0
    }

    #[test]
    fn stop_refuses_gate_execution_without_owned_tasks() {
        let _heap = setup();
        let id = ready_component(b"exit_gate", 0);
        registry::get_registry().lock().begin_call(id).unwrap();
        assert_eq!(
            stop_component(id),
            Err(ComponentStopError::ActiveExecutions)
        );
        let mut reg = registry::get_registry().lock();
        assert_eq!(reg.get(id).unwrap().state, ComponentState::Ready);
        assert_eq!(reg.active_calls(id), 1);
        reg.finish_call(id);
        reg.begin_stop(id).unwrap();
        assert_eq!(reg.begin_call(id), Err(RegistryError::NotReady));
    }

    #[test]
    fn direct_publication_pins_ctx_even_after_endpoint_invalidation() {
        use crate::component::abi::{InterfaceAbi, InterfaceKind};
        use crate::component::endpoint::{self, ContractId};
        let _heap = setup();
        let id = ready_component(b"exit_direct", 0);
        let mut state = 7u32;
        {
            let reg = registry::get_registry().lock();
            let mut endpoints = endpoint::get_endpoints().lock();
            endpoints
                .stage_publish(
                    &reg,
                    id,
                    b"direct",
                    ContractId::from_raw(0xE217),
                    InterfaceKind::Service,
                    InterfaceAbi::from_raw(1),
                    0,
                    core::ptr::from_ref(&state).cast(),
                    core::ptr::from_mut(&mut state).cast(),
                )
                .unwrap();
            endpoints.commit_pending(&reg, id).unwrap();
            endpoints.invalidate_provider(id);
        }
        assert_eq!(stop_component(id), Err(ComponentStopError::DirectExports));
        assert_eq!(
            registry::get_registry().lock().get(id).unwrap().state,
            ComponentState::Ready
        );
        assert_eq!(state, 7);
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
        // `stop_component` 安装组件边界（Exit guard）覆盖进程全局 `ACTIVE_GUARD`，
        // 必须与其它触碰边界栈的测试串行（rank = BOUNDARY，先于 memory GUARD）。
        let _boundary = containment::test_boundary_lock();
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
        // 同 `stop_drives_ready_to_stopped_through_destroy_entry`：成功的 stop 会
        // 安装 Exit 边界，必须持 BOUNDARY 锁。
        let _boundary = containment::test_boundary_lock();
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
    fn stop_backstop_revokes_leftover_device() {
        // 同 `stop_drives_ready_to_stopped_through_destroy_entry`：成功的 stop 会
        // 安装 Exit 边界，必须持 BOUNDARY 锁（rank 0，先于 machine / memory GUARD）。
        let _boundary = containment::test_boundary_lock();
        // Given：Ready 组件 + 一台它没来得及释放的设备。
        let _machine = crate::machine::test_support::GUARD.lock();
        let _heap = setup();
        commit_devices(&[(23, "exit,mmio")]);
        let id = ready_component(b"exit_leftover_mmio", destroy_hook_ok as *const () as usize);
        let ctx = RequestContext {
            component: id,
            task: None,
        };
        crate::resource::device::claim(&ctx, crate::machine::DeviceId::from_raw(23)).unwrap();

        // When：停止（销毁入口什么也没释放）。
        assert_eq!(stop_component(id), Ok(()));

        // Then：Core 兜底撤销了剩余 ownership 并 quarantine（与 failure 路径同一序列）。
        assert!(
            crate::resource::device::get_table()
                .lock()
                .is_quarantined(crate::machine::DeviceId::from_raw(23))
        );
        assert!(
            !crate::resource::device::get_table()
                .lock()
                .owner(crate::machine::DeviceId::from_raw(23))
                .is_some()
        );

        // 清理：进程全局 quarantine 标记不能在用例间残留。
        crate::resource::device::get_table()
            .lock()
            .clear_quarantine();
    }

    /// 契约核心：停止**只影响被选中的组件**——同 artifact 的另一个组件保持
    /// Ready，其设备 ownership 不受影响。
    #[test]
    fn stop_affects_only_the_selected_instance() {
        // 同 `stop_drives_ready_to_stopped_through_destroy_entry`：成功的 stop 会
        // 安装 Exit 边界，必须持 BOUNDARY 锁（rank 0，先于 machine / memory GUARD）。
        let _boundary = containment::test_boundary_lock();
        // Given：同一 artifact 的两个 Ready 组件，各自认领一台设备。
        let _machine = crate::machine::test_support::GUARD.lock();
        let _heap = setup();
        commit_devices(&[(10, "exit,mmio0"), (11, "exit,mmio1")]);
        let destroy = destroy_hook_ok as *const () as usize;
        let (first, second) = {
            let mut reg = registry::get_registry().lock();
            let first = reg
                .declare(
                    b"exit_two_instances",
                    test_loaded(destroy),
                    ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(first).unwrap();
            reg.begin_start(first).unwrap();
            reg.finish_start(first).unwrap();
            let second = reg
                .declare(
                    b"exit_two_instances",
                    test_loaded(destroy),
                    ExecutionDomain::KernelNative,
                )
                .unwrap();
            reg.resolve(second).unwrap();
            reg.begin_start(second).unwrap();
            reg.finish_start(second).unwrap();
            (first, second)
        };
        assert_ne!(first, second, "两个实例身份不同");
        let first_ctx = RequestContext {
            component: first,
            task: None,
        };
        crate::resource::device::claim(&first_ctx, crate::machine::DeviceId::from_raw(10)).unwrap();
        let second_ctx = RequestContext {
            component: second,
            task: None,
        };
        crate::resource::device::claim(&second_ctx, crate::machine::DeviceId::from_raw(11))
            .unwrap();

        // When：只停止第一个实例。
        assert_eq!(stop_component(first), Ok(()));

        // Then：第一个 Stopped + 设备 quarantine；第二个仍 Ready + 设备仍归它。
        let reg = registry::get_registry().lock();
        assert_eq!(reg.get(first).unwrap().state, ComponentState::Stopped);
        assert_eq!(reg.get(second).unwrap().state, ComponentState::Ready);
        drop(reg);
        let table = crate::resource::device::get_table().lock();
        assert!(table.is_quarantined(crate::machine::DeviceId::from_raw(10)));
        assert!(!table.is_quarantined(crate::machine::DeviceId::from_raw(11)));
        assert_eq!(
            table.owner(crate::machine::DeviceId::from_raw(11)),
            Some(second),
            "未选中实例的 ownership 必须原样"
        );
        drop(table);

        // 清理：清掉进程全局 quarantine 标记。
        crate::resource::device::get_table()
            .lock()
            .clear_quarantine();
    }

    /// 停止路径（与失败路径共用 Core 兜底）使被停实例的 endpoint 永久失效；
    /// 其它实例的 endpoint 不受影响。
    #[test]
    fn stop_invalidates_only_the_stopped_instances_endpoints() {
        use crate::component::abi::{InterfaceAbi, InterfaceKind};
        use crate::component::endpoint::{self, ContractId, EndpointError, EndpointState};

        // 同 `stop_drives_ready_to_stopped_through_destroy_entry`：成功的 stop 会
        // 安装 Exit 边界，必须持 BOUNDARY 锁。
        let _boundary = containment::test_boundary_lock();
        let _heap = setup();
        const CONTRACT: ContractId = ContractId::from_raw(0xE0D0_5001);
        const ENDPOINT_ABI: InterfaceAbi = InterfaceAbi::from_raw(0xE0D0_5002);

        // Given：两个 Ready 实例各自发布一条 endpoint。
        let first = ready_component(
            b"exit_endpoint_first",
            destroy_hook_ok as *const () as usize,
        );
        let second = ready_component(
            b"exit_endpoint_second",
            destroy_hook_ok as *const () as usize,
        );
        let (first_endpoint, second_endpoint) = {
            let reg = registry::get_registry().lock();
            let mut eps = endpoint::get_endpoints().lock();
            eps.stage_publish(
                &reg,
                first,
                b"blk0",
                CONTRACT,
                InterfaceKind::Device,
                ENDPOINT_ABI,
                1,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
            eps.commit_pending(&reg, first).unwrap();
            eps.stage_publish(
                &reg,
                second,
                b"blk1",
                CONTRACT,
                InterfaceKind::Device,
                ENDPOINT_ABI,
                2,
                core::ptr::null(),
                core::ptr::null_mut(),
            )
            .unwrap();
            eps.commit_pending(&reg, second).unwrap();
            (
                eps.discover(&reg, first, b"blk0", CONTRACT).unwrap(),
                eps.discover(&reg, second, b"blk1", CONTRACT).unwrap(),
            )
        };

        // When：只停止第一个实例。
        assert_eq!(stop_component(first), Ok(()));

        // Then：第一个的 endpoint 永久 dead；第二个仍 Live。
        let reg = registry::get_registry().lock();
        let eps = endpoint::get_endpoints().lock();
        assert_eq!(
            eps.lookup(&reg, first_endpoint, CONTRACT, ENDPOINT_ABI),
            Err(EndpointError::EndpointDead)
        );
        assert_eq!(
            eps.discover(&reg, first, b"blk0", CONTRACT),
            Err(EndpointError::EndpointDead)
        );
        assert_eq!(
            eps.lookup(&reg, second_endpoint, CONTRACT, ENDPOINT_ABI)
                .unwrap()
                .state,
            EndpointState::Live
        );
    }
}
