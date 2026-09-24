//! ComponentId 与 ResourceDomain：组件身份、资源集合、最终回收。
//! 回收不预设 universal revoke order（graceful shutdown / forced containment 双路径，
//! 见 docs/architecture/component-model.md §3）：Core 保证 eventual revocation，
//! 具体设备 shutdown 顺序由组件/驱动决定，不由 ResourceDomain 写死。
//! ResourceDomain 的实现由人类完成；本模块只提供词汇表占位与 host test 样板。

pub mod abi;
pub mod call;
pub mod containment;
mod elf;
pub mod endpoint;
pub mod exit;
pub mod export;
pub mod failure;
pub mod image;
/// 私有 AS 切换网关的 Core 侧准备（`isolated_lifecycle` 生产调用；仅
/// S-mode + MMU + RISC-V 目标有意义，其余 profile 不提供、也不降级）。
#[cfg(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
pub mod isolated;
/// Isolated 域**实例生命周期**：私有 AS + 按域镜像 + Core 预置实例窗口 +
/// runtime slot，经 assembly gateway 执行 `kcomp_instance_create` /
/// `kcomp_instance_destroy`。无私有 AS backend 的构建显式拒绝，绝不降级。
/// 同一模块还承载跨域 service dispatch（KernelNative caller → Isolated
/// provider，经扁平帧邮箱 + gateway）。
pub mod isolated_lifecycle;
/// 按域装载：把一个已解析的 `.kcomp` 的段按页级权限放进实例的私有 AS。
/// 由 `isolated_lifecycle` 调用。
pub mod isolated_load;
/// Isolated 跨域 service 的**扁平调用帧邮箱**：caller frame → Core 拥有的邮箱
/// backing → provider 域内 VA；容量固定、超长显式拒绝。
pub mod isolated_mailbox;
pub mod load;
pub mod loader;
pub mod registry;
pub mod runtime_slot;
pub mod store;

pub use containment::panic_escape;
pub use exit::{ComponentStopError, stop_component};
pub use failure::fail_component;
pub use image::{ComponentImage, ComponentImageId};

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

/// Core 真相查询：`id` 是否运行在 [`endpoint::ExecutionDomain::KernelNative`]。
///
/// 只有 KernelNative 具备**已实现**的 Core ABI 资源 / 调用路径（MMIO、DMA、
/// IRQ、任务、出站服务调用）。Isolated / Sandbox 的对应机制尚未实现，所有
/// acquiring 入口据此**显式拒绝**（`-ENOTSUP`），绝不静默跨域降级。
///
/// 未声明的身份回退 `KernelNative`（`endpoint::instance_domain` 的既有防御
/// 语义）；需要存在性校验的调用方必须另行解析 caller。
pub fn is_kernel_native(id: ComponentId) -> bool {
    let registry = registry::get_registry().lock();
    endpoint::instance_domain(&registry, id) == endpoint::ExecutionDomain::KernelNative
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
    /// 所有 required Endpoints 都已成功绑定。
    /// 语义：Resolved = 依赖已就位，可以进入初始化。
    Resolved,
    Starting,
    Ready,
    /// 正在停止：组件销毁入口 `kcomp_instance_destroy(state)`（Linux
    /// `module_exit` 类比）执行期，由 `component/exit.rs::stop_component` 驱动
    /// （`Ready → Stopping`）。
    ///
    /// 此状态下 `may_run` 不再放行该实例的任务，`kcore_endpoint_publish` 也
    /// 不再接受（destroy 边界不是 publish principal）；已有 authority 仍可由钩子
    /// 自行 `release`（teardown 不受生命周期门禁限制，见 `export.rs`）。
    Stopping,
    /// 已停止：`kcomp_instance_destroy` 已返回 0、剩余 authority 与 endpoint 已由
    /// Core 兜底回收（`Stopping → Stopped`，由 `stop_component` 提交）。
    ///
    /// 保留记录：不回收段内存、不退役实例、`ComponentId` 不复用。
    Stopped,
    /// 运行过程中失败（逻辑死亡，可触发恢复流程）。
    ///
    /// 意外退出 / abort **当前统一由 `Failed` 覆盖**（组件 panic containment
    /// 路径提交 `Failed`）。
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
    /// `Ready → Stopping → Stopped` 由 `component/exit.rs::stop_component` 的停止
    /// 编排驱动（`Registry::begin_stop` / `finish_stop`）。`Failed` 目标从任意
    /// 状态均合法，保持 `mark_failed` 的既有语义（可重复标记、保持 `Failed`）。
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

    /// 实例是否仍**活着**（未到终态）：`Declared` / `Resolved` / `Starting` /
    /// `Ready` / `Stopping`。
    ///
    /// 终态 = `Stopped` / `Failed`（tombstone，记录保留）。
    /// 用途：Isolated 的同 image 并发门禁（`component/load.rs`）——逻辑重启只在
    /// 前一个实例**逻辑死亡之后**成立；两个活跃实例会共享 image 的
    /// `.data` / `.bss`（image-global，与 KernelNative 同一契约），不在当前
    /// 承诺的隔离模型内。
    pub const fn is_live(self) -> bool {
        matches!(
            self,
            Self::Declared | Self::Resolved | Self::Starting | Self::Ready | Self::Stopping
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ComponentState;

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

    /// `is_live` 与状态机终态精确互补：只有 `Stopped` / `Failed` 是 tombstone。
    #[test]
    fn is_live_marks_exactly_the_non_terminal_states() {
        use ComponentState::{Declared, Failed, Ready, Resolved, Starting, Stopped, Stopping};

        for state in [Declared, Resolved, Starting, Ready, Stopping] {
            assert!(state.is_live(), "{state:?} must be live");
        }
        for state in [Stopped, Failed] {
            assert!(!state.is_live(), "{state:?} must be a tombstone");
        }
    }
}
