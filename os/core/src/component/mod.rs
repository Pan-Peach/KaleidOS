//! ComponentId 与 ResourceDomain：组件身份、资源集合、最终回收。
//! 回收不预设 universal revoke order（graceful shutdown / forced containment 双路径，
//! 见 docs/component-model.md §3）：Core 保证 eventual revocation，
//! 具体设备 shutdown 顺序由组件/驱动决定，不由 ResourceDomain 写死。
//! ResourceDomain 的实现由人类完成；本模块只提供词汇表占位与 host test 样板。

pub mod containment;
mod elf;
pub mod exit;
pub mod export;
pub mod failure;
pub mod interface;
pub mod load;
pub mod loader;
pub mod registry;
pub mod store;

pub use containment::panic_escape;
pub use exit::stop_component;
pub use failure::fail_component;

/// Core 真相门禁：`id` 是否为 `Failed`（逻辑死亡）实例。
///
/// 失败实例不得获取新 authority 或创建新 work；`release` / `revoke` 等 teardown
/// 操作不受此门禁限制。
pub fn is_failed(id: ComponentId) -> bool {
    registry::get_registry().lock().is_failed(id)
}

/// Core 真相门禁：`id` 拥有的任务是否允许运行（活实例 = `Starting` / `Ready`）。
pub fn may_run(id: ComponentId) -> bool {
    registry::get_registry().lock().may_run(id)
}

/// 组件身份（M1 最小词汇表）—— **Identity，不是 Authority**。
/// 由 Core 分配；组件的 ResourceDomain 以 ComponentId 为键记录。
/// 可被猜测/构造/传递，但真正的操作权限来自 Core 授予的组件凭证（token），
/// 任何来自 Component/IPC/Wasm 的 ID 都要过 Core 验证。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComponentId(u32);

impl ComponentId {
    /// ID 是身份标识，不是授权：可从 raw 值构造、可序列化/传递。
    /// 来自 Component/IPC/Wasm 的 ID 必须由 Core 重新验证。
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// 原始编号（供 Core 记录与 trace 使用）。
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComponentState {
    Declared,
    /// 所有 required Interfaces 都已成功绑定（见 `component/interface.rs`）。
    /// 语义：Resolved = 依赖已就位，可以进入初始化。
    Resolved,
    Starting,
    Ready,
    /// 正在停止：**shape-only stub**——本增量没有任何路径进入此状态，
    /// `Stopping → Stopped` 的转换也尚未接线。
    ///
    /// 预期（future）语义：组件退出入口 `kcomp_exit`（Linux `module_exit` 类比）
    /// 执行期，publish/claim 等新 work 被拒绝，已有状态逐步清理（quiesce/drain）。
    ///
    /// TODO(component-exit): ComponentManager 的 stop 路径落地后
    /// （`Ready → Stopping → Stopped`），由它调用 `ComponentRecord.exit`。
    Stopping,
    /// 已停止：**shape-only stub**——本增量没有任何路径进入此状态。
    ///
    /// 预期（future）语义：`kcomp_exit` 已返回、实例的 authority 与接口已回收，
    /// 之后可退休该实例（释放名字槽、分配新 `ComponentId`）并重新探测。
    Stopped,
    /// 运行过程中失败（逻辑死亡，可触发恢复流程）。
    ///
    /// 意外退出 / abort **当前统一由 `Failed` 覆盖**（组件 panic containment
    /// 路径提交 `Failed`）。
    ///
    /// TODO(unexpected-exit): 失败实例的状态提交点（`containment` 的
    /// task-abort trampoline → `sched::abort_current_task` → `fail_component`）
    /// 就是未来"独立 abort/exit 通知"的 hook 点；届时可区分普通失败与组件
    /// 主动退出。
    Failed,
}

impl ComponentState {
    /// 合法生命周期转换的**唯一真相**（Core owns truth）。
    ///
    /// 非法转换返回 `false`；`Registry` 的唯一转换入口据此拒绝并保持原状态，
    /// 各转换方法不再各自硬编码 `state != X`。转移表：
    ///
    /// ```text
    /// Declared  → Resolved
    /// Resolved  → Starting
    /// Starting  → Ready
    /// Ready     → Stopping
    /// Stopping  → Stopped
    /// 任意状态  → Failed          （逻辑死亡；含 Failed → Failed 幂等）
    /// ```
    ///
    /// `Ready → Stopping → Stopped` 已在此声明为规则，但当前**没有任何生产路径
    /// 驱动它们**（stop orchestration 仍 deferred；`Registry::begin_stop` /
    /// `finish_stop` 是仅声明未接线的入口）。`Failed` 目标从任意状态均合法，
    /// 保持 `mark_failed` 的既有语义（可重复标记、保持 `Failed`）。
    pub const fn can_transition(self, to: Self) -> bool {
        if matches!(to, Self::Failed) {
            return true;
        }
        matches!(
            (self, to),
            (Self::Declared, Self::Resolved)
                | (Self::Resolved, Self::Starting)
                | (Self::Starting, Self::Ready)
                | (Self::Ready, Self::Stopping)
                | (Self::Stopping, Self::Stopped)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ComponentId, ComponentState};

    #[test]
    fn ids_with_same_raw_are_equal() {
        assert_eq!(ComponentId::from_raw(3), ComponentId::from_raw(3));
    }

    #[test]
    fn ids_with_different_raw_differ() {
        assert_ne!(ComponentId::from_raw(3), ComponentId::from_raw(4));
    }

    #[test]
    fn raw_roundtrip() {
        assert_eq!(ComponentId::from_raw(17).raw(), 17);
    }

    #[test]
    fn can_transition_matches_lifecycle_matrix() {
        use ComponentState::{Declared, Failed, Ready, Resolved, Starting, Stopped, Stopping};

        const STATES: [ComponentState; 7] = [
            Declared, Resolved, Starting, Ready, Stopping, Stopped, Failed,
        ];
        // 唯一合法的非 Failed 边；`Failed` 目标对任意状态都合法（见实现文档）。
        const LEGAL: [(ComponentState, ComponentState); 5] = [
            (Declared, Resolved),
            (Resolved, Starting),
            (Starting, Ready),
            (Ready, Stopping),
            (Stopping, Stopped),
        ];

        for from in STATES {
            for to in STATES {
                let expected = LEGAL.contains(&(from, to)) || to == Failed;
                assert_eq!(
                    from.can_transition(to),
                    expected,
                    "{from:?} -> {to:?} legality"
                );
            }
        }
    }

    #[test]
    fn failed_is_reachable_from_every_state_and_idempotent() {
        for from in [
            ComponentState::Declared,
            ComponentState::Resolved,
            ComponentState::Starting,
            ComponentState::Ready,
            ComponentState::Stopping,
            ComponentState::Stopped,
            ComponentState::Failed,
        ] {
            assert!(
                from.can_transition(ComponentState::Failed),
                "{from:?} -> Failed must be legal"
            );
        }
    }
}
