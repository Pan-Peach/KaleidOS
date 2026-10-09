//! 组件实例创建语义入口（ComponentManager 教学版占位）：仓库读取 → loader 放段 /
//! 重定位 → registry 声明组件（组件 1:1 拥有自己的 loaded image）→ resolve →
//! begin_start（Starting）→ 调用 `kcomp_instance_create(args, &out_state)` → 记录
//! state → 原子提交 pending endpoints → finish_start（Ready）。
//!
//! `monitor load <name>` 与组件 ABI `kcore_component_load` 都是这里的**薄 caller**——
//! 加载流程本身属于 Core（monitor 不是 ComponentManager）。完整依赖解析、
//! kpkg manifest requires、失败回滚留给真正的 ComponentManager 里程碑。
//!
//! # 每次 instantiate = 一个完整运行组件（`docs/architecture/component-model.md`）
//!
//! **一次 load / instantiate = 一个 `ComponentId`**：同名 artifact 再次创建会
//! **重新放段并重定位**，得到全新的 writable image state（独立的 `.data` / `.bss`）
//! ——不再按 artifact 名复用镜像。`.kcomp` 是 artifact（程序），不是运行实例。
//! `restart` = 从同一个 artifact 再 instantiate 一个新组件（新 id、新 backing）。
//! 重复代码页的共享是**未来 loader / MM 优化**，不是组件语义模型。

use crate::component::containment::{self, CallOutcome, KcompCreateArgs};
use crate::component::elf::ElfObject;
use crate::component::endpoint::{self, EndpointError, ExecutionDomain};
use crate::component::isolated_lifecycle;
use crate::component::loader::{self, LoaderError};
use crate::component::{ComponentId, failure, registry};
use crate::errno::Errno;
use crate::memory::address_space;
use crate::task::TaskId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentLoadError {
    /// 仓库未挂载（boot 未内嵌 init.kpkg）。
    StoreNotMounted,
    /// 仓库里没有 `<name>.kcomp`。
    NotFound,
    /// 仓库读失败。
    ReadFailed,
    /// ELF 解析 / 放段 / 重定位 / 必需符号（create/destroy/abi）校验失败。
    Loader(LoaderError),
    /// registry 声明组件失败（id 耗尽）。
    DeclareFailed,
    /// resolve 失败（require 未满足；v1 无 requires，不应发生）。
    ResolveFailed,
    /// 状态机拒绝 begin_start/finish_start。
    StartFailed,
    /// `kcomp_instance_create` 返回非零（`0 / -errno` 约定，失败位图约定已废弃）。
    CreateFailed(i32),
    /// `kcomp_instance_create` panic 已切回 Core；实例由 caller 提交为 Failed。
    CreatePanicked,
    /// `kcomp_instance_destroy` 返回非零（实例 → `Failed` + 兜底；**绝不自动重试**）。
    /// 由 `component/exit.rs::stop_component` 作为失败原因传入。
    DestroyFailed(i32),
    /// `kcomp_instance_destroy` panic，已由 Destroy 边界切回 Core（同上）。
    DestroyPanicked,
    /// create 返回 0，但 pending endpoints 提交冲突（契约 kind / abi、
    /// 端口名重复、id 容量）——实例被提交为 Failed；旧 endpoint 不受影响。
    EndpointCommitFailed(EndpointError),
    /// 组件拥有的任务 panic，已由 task-abort 上下文提交为 `Exited`；
    /// 组件的 authority 由 abort 路径撤销（仅作 reason 语义）。
    TaskPanicked(TaskId),
    /// 组件作为 provider 的 `kcomp_service_dispatch` 在 service-call 边界内
    /// panic，已切回 caller 的 Core 帧；`component/call.rs` 据此把 provider 提交
    /// 为 `Failed`（caller 不受影响）。
    ServicePanicked,
    /// 调度策略 provider 在 **PolicyCall 边界**内 panic，已逃逸回挂起的调度帧；
    /// `component/call.rs` 据此把 provider 提交为 `Failed`（调度继续用确定性回退）。
    PolicyPanicked,
    /// 调度策略 provider 的提议不可用（不在 Core 的 runnable 快照内 / 返回非 0）：
    /// provider 被隔离（逻辑死亡），Core 退役该策略并用确定性回退继续调度。
    PolicyRejected,
    /// 在 **policy 回调**（`PolicyCall` 边界，含其下的嵌套边界）内请求创建组件：
    /// 策略回调有界（不得阻塞 / 不得分配 / 不得创建组件），Core 拒绝 → `-EINVAL`。
    InPolicyContext,
    /// IRQ callbacks cannot enter the allocating/loading lifecycle path.
    InIrqContext,
    /// Failed / stopped / unknown caller cannot create new work.
    CallerNotReady,
    /// 请求 `IsolatedNative` 部署，但当前平台 / profile **没有私有地址空间能力**
    /// （NoMMU 恒等 backend，或没有真实 backend）：`AddressSpaceBackend` 可用
    /// **不等于**有隔离能力 → `-ENOTSUP`，绝不把恒等映射当私有 AS 用。
    IsolationUnsupported,
    /// SandboxedNative 的 U-mode / task-AS / syscall 机制尚未实现：装载前 ENOTSUP。
    SandboxUnsupported,
    /// `IsolatedNative` 装载发现**不在支持白名单里的 `kcore_*` import**：
    /// 支持诊断 / 只读、panic、私有 backing 与 endpoint API
    /// （[`crate::component::isolated_load::SUPPORTED_IMPORTS`]），面外符号在装载
    /// **之前**拒绝——绝不回退到 KernelNative 的裸 Core 函数地址。
    IsolatedImportUnsupported,
    /// `IsolatedNative` 按域放段失败（段出窗 / 重叠 / 权限不可表达 / 入口不可执行 /
    /// 地址溢出 / import 白名单之外的具名 UNDEF 符号 / 后端拒绝映射）：镜像不适配
    /// 该域，显式拒绝（`kcore_*` 面外 import 由门禁以 `-ENOTSUP` 区分）。
    IsolatedPlacementFailed,
    /// `IsolatedNative` 的 config 负载放不进实例窗口（或指针 / 长度不自洽）：
    /// 显式拒绝，绝不截断。
    IsolatedConfigRejected,
    /// Isolated 的 `kcomp_instance_create` 在私有 AS 内故障，由 Core 的
    /// 故障分派判为不可恢复（`Outcome::Faulted`）；实例未完整构造、不调用 destroy。
    CreateFaulted,
    /// Isolated provider 在**跨 AS service dispatch** 期间故障，由 Core 的
    /// 故障分派判为不可恢复（`Outcome::Faulted`）：provider 逻辑死亡 + AS 退役 +
    /// Core 预置窗口归还，caller 存活。
    ServiceFaulted,
}

impl ComponentLoadError {
    /// ABI 边界状态码：**保留** `kcomp_instance_create` 返回的原始 `-errno`。
    ///
    /// 组成链上的 caller（尤其 driver prober 的创建失败记录）需要真实原因：
    /// `-EBUSY`（独占设备 / 第二次 attachment）、`-EINVAL`（config 非法）不能被
    /// 塌缩成 `EIO`。正数非零违反 `0 / -errno` 约定 → `EIO`（组件违约，不是
    /// 有意义的 errno）；其余臂沿用 [`Errno::from`] 的映射。
    pub fn abi_status(self) -> i32 {
        match self {
            Self::CreateFailed(code) if code < 0 => code,
            Self::CreateFailed(_) => Errno::EIO.code(),
            other => Errno::from(other).code(),
        }
    }
}

/// 当前 CPU 正在创建的实例（create 调用期间由 Core 记录）。
///
/// `kcore_endpoint_publish` 的 provider 以及锚点上
/// create 阶段的 task requester 从这里解析——组件不需要知道自己/别人的
/// ComponentId，Core 不信任组件自报的身份。普通任务的 requester 从
/// `TaskRecord.owner` 解析。嵌套创建（组件 create 里再创建别的组件）时保存/恢复。
/// 取当前正在创建的实例；不在 create 调用内返回 None。
pub fn current_component() -> Option<ComponentId> {
    containment::creating()
}

/// 在本 CPU 的 creating = Some(component) 上下文里执行 `f`（返回后恢复嵌套前的值）。
///
/// 与 KernelNative create 同一身份纪律：组件在 Core 拥有的边界内看到自己是
/// "正在被创建的实例"；嵌套调用（组件 create 里再创建组件）保存 / 恢复。
/// Core 内部 API（不在组件导出白名单里）。
pub fn with_current<R>(component: ComponentId, f: impl FnOnce() -> R) -> R {
    let previous = containment::creating();
    containment::replace_creating(Some(component));
    let result = f();
    containment::replace_creating(previous);
    result
}

/// 用默认配置 + 指定部署域创建一个实例（无 config 负载）。
///
/// 组件 ABI `kcore_component_load` 与 monitor `load <name> [kind]` 的便利入口；
/// 等价于 [`create_component`] + [`KcompCreateArgs::empty`]。
/// `kind` 是部署请求（不是 authority）：Core 在 [`create_component`] 里按域分派后才提交。
pub fn load_and_start(
    name: &[u8],
    kind: ExecutionDomain,
) -> Result<ComponentId, ComponentLoadError> {
    create_component(name, &KcompCreateArgs::empty(), kind)
}

/// 用指定 config 负载 + 部署域创建一个新实例，返回实例 id。
///
/// `kind` 是**部署请求**（Policy proposes）：本函数**按执行域分派**创建路径——
/// `KernelNative` 走 [`create_kernel_native`]（现有完整创建链）；`IsolatedNative`
/// 走 [`create_isolated_native`] 的门禁（能力 / import 白名单任一不满足即显式拒绝）
/// 后交 `isolated_lifecycle` 真正创建；`SandboxedNative` 走 [`create_sandboxed_native`] 占位，
/// 装载前返回 ENOTSUP。任何域都**绝不静默降级成 native 跑**
/// （`docs/architecture/deployment.md` §2 ⑤/§10）。
///
/// 锁纪律：registry / endpoint 锁只覆盖各自的查询与提交；`kcomp_instance_create`
/// 在**无锁**状态下调用（组件 create 可能再创建别的组件、publish 接口、创建任务，
/// 都各自拿锁——不能有任何锁跨组件调用持有）。
pub fn create_component(
    name: &[u8],
    args: &KcompCreateArgs,
    kind: ExecutionDomain,
) -> Result<ComponentId, ComponentLoadError> {
    // 上下文门禁：调度策略回调有界——嵌套组件创建（含藏在嵌套生命周期边界
    // 之下）一律拒绝，绝不把组件加载链伸进策略执行。
    if containment::policy_call_in_chain() {
        return Err(ComponentLoadError::InPolicyContext);
    }

    if containment::irq_in_chain() {
        return Err(ComponentLoadError::InIrqContext);
    }
    let caller = crate::resource::RequestContext::ambient().map(|ctx| ctx.component);
    if caller.is_some_and(|id| !registry::get_registry().lock().may_run(id)) {
        return Err(ComponentLoadError::CallerNotReady);
    }

    // 按执行域分派：每个域一个创建入口——接入新域 = 新增一个臂，而不是"放开一道
    // guard"（guard 会让"未实现域"与"已实现域"共用同一条装载路径，混淆真相）。
    match kind {
        ExecutionDomain::KernelNative => create_kernel_native(name, args),
        // Isolated 有真实执行器：`create_isolated_native` 建私有 AS、放置镜像，
        // 再经跨 AS trampoline 跑 `kcomp_instance_create`（见 `isolated_lifecycle`）。
        ExecutionDomain::IsolatedNative => create_isolated_native(name, args),
        // Sandbox 执行器未实现（U-mode + 私有 AS + ecall）。
        ExecutionDomain::SandboxedNative => create_sandboxed_native(name, args),
    }
}

/// Revalidate the creator and publish the new instance under one registry lock.
/// Loading/placement occurs before this point; component code runs after unlock.
pub(crate) fn declare_instance(
    name: &[u8],
    loaded: loader::LoadedComponent,
    domain: ExecutionDomain,
) -> Result<ComponentId, ComponentLoadError> {
    let caller = crate::resource::RequestContext::ambient().map(|ctx| ctx.component);
    let mut reg = registry::get_registry().lock();
    if caller.is_some_and(|id| !reg.may_run(id)) {
        return Err(ComponentLoadError::CallerNotReady);
    }
    let id = reg
        .declare(name, loaded, domain)
        .map_err(|_| ComponentLoadError::DeclareFailed)?;
    reg.record_creator(id, caller)
        .map_err(|_| ComponentLoadError::DeclareFailed)?;
    Ok(id)
}

/// `KernelNative` 的创建路径。
///
/// 生命周期：`Declared → resolve → Resolved → begin_start → Starting →
/// kcomp_instance_create → { failure → Failed | success → record state →
/// commit pending endpoints → Ready }`。每次 instantiate 都从 artifact 重新放段 /
/// 重定位；组件 1:1 拥有自己的 loaded image。
fn create_kernel_native(
    name: &[u8],
    args: &KcompCreateArgs,
) -> Result<ComponentId, ComponentLoadError> {
    // 每次 instantiate 都**重新放段 + 重定位**：这个组件拥有自己独立的
    // writable image state（`.data` / `.bss` 不共享）。
    let blob = read_artifact(name)?;
    let loaded = loader::load_component(&blob).map_err(ComponentLoadError::Loader)?;
    let create_entry = loaded.create;
    let runtime_entry = loaded.runtime_init;

    let id = {
        let id = declare_instance(name, loaded, ExecutionDomain::KernelNative)?;
        let mut reg = registry::get_registry().lock();
        reg.resolve(id)
            .map_err(|_| ComponentLoadError::ResolveFailed)?;
        // Resolved → Starting：`kcomp_instance_create` 执行期间 publish 只记录 pending。
        reg.begin_start(id)
            .map_err(|_| ComponentLoadError::StartFailed)?;
        id
    };

    // 入口调用：期间本 CPU 的 creating = 本实例（publish / task_create 的身份来源）。
    // Core 先把 out_state 置 NULL（无状态组件可成功写回 NULL）。
    let mut instance_state: *mut () = core::ptr::null_mut();
    let previous = containment::creating();
    containment::replace_creating(Some(id));
    let outcome = match runtime_entry.map(|entry| {
        containment::call_component_runtime(
            entry,
            &crate::component::export::runtime_backend(ExecutionDomain::KernelNative),
        )
    }) {
        None | Some(CallOutcome::Returned(0)) => {
            containment::call_component_create(create_entry, args, &mut instance_state)
        }
        Some(outcome) => outcome,
    };
    containment::replace_creating(previous);

    match outcome {
        CallOutcome::Returned(0) => {
            // Core 记录组件写回的 opaque state（成功之后、commit 之前）。
            if registry::get_registry()
                .lock()
                .record_instance_state(id, instance_state)
                .is_err()
            {
                // 刚声明的实例必然存在：失败 = Core 不变式破坏（不应发生）。
                let error = ComponentLoadError::StartFailed;
                failure::fail_component(id, error);
                return Err(error);
            }
            // create 成功：原子提交 pending endpoints——成功后进入 Ready。提交
            // 失败交给 fail_component 兜底（已提交的 endpoint 会被永久失效），
            // 旧 provider 的真相不受影响。
            let endpoint_commit = {
                let reg = registry::get_registry().lock();
                endpoint::get_endpoints().lock().commit_pending(&reg, id)
            };
            let commit_error = endpoint_commit
                .err()
                .map(ComponentLoadError::EndpointCommitFailed);
            match commit_error {
                None => {
                    let ready = registry::get_registry().lock().finish_start(id).is_ok();
                    if ready {
                        Ok(id)
                    } else {
                        // Starting → Ready 失败是 Core 不变式破坏（不应发生）。
                        let error = ComponentLoadError::StartFailed;
                        failure::fail_component(id, error);
                        Err(error)
                    }
                }
                Some(error) => {
                    // create 成功但 commit 失败：按"物理驻留"原则保守保留 state，
                    // 不调用 destroy（组件未完整进入 Ready）。
                    failure::fail_component(id, error);
                    Err(error)
                }
            }
        }
        CallOutcome::Returned(code) => {
            // create 失败：组件自己负责内部错误清理；Core **不调用 destroy**。
            let error = ComponentLoadError::CreateFailed(code);
            failure::fail_component(id, error);
            Err(error)
        }
        CallOutcome::NoStack => {
            // 边界栈分配失败（Core 侧 `-ENOMEM`）：入口从未执行，等价 create 失败。
            let error = ComponentLoadError::CreateFailed(containment::STACK_ALLOCATION_FAILED);
            failure::fail_component(id, error);
            Err(error)
        }
        CallOutcome::Panicked => {
            // panic 的实例未完整构造：同样不调用 destroy。
            let error = ComponentLoadError::CreatePanicked;
            failure::fail_component(id, error);
            Err(error)
        }
    }
}

/// `IsolatedNative` 的创建路径（真正创建、启动、销毁）。
///
/// 本函数只做**装载前置门禁**（平台能力 + import 白名单），随后把创建编排交给
/// [`isolated_lifecycle::create`]：
///
/// 1. **平台能力**：当前 profile 必须有私有地址空间 backend（NoMMU / 无后端 → 拒绝）；
/// 2. **import 白名单**：不得含未支持的 `kcore_*` / 具名 UNDEF 符号
///    （见 [`validate_isolated_load`]）；
/// 3. 私有 AS + 按域放段 + Core 预置窗口（栈 / 实例窗口）；
/// 4. `kcomp_instance_create` 在私有 AS 内经跨 AS trampoline 执行 → `Ready`；
///    任一步失败 = 退役 AS + 归还窗口 backing + `Failed`（半成品不留）。
fn create_isolated_native(
    name: &[u8],
    args: &KcompCreateArgs,
) -> Result<ComponentId, ComponentLoadError> {
    // (1) 能力门禁：trait 可用 ≠ 隔离能力（NoMMU 也实现 AddressSpaceBackend）。
    if !address_space::isolation_capable() {
        return Err(ComponentLoadError::IsolationUnsupported);
    }

    // (2) 装载前置门禁；blob 只读一次，随后的按域装载用同一份字节。
    let blob = validate_isolated_load(name)?;

    // (3) 生命周期编排。
    isolated_lifecycle::create(name, &blob, args)
}

/// `SandboxedNative` 的创建入口；执行器尚未实现，装载前返回 ENOTSUP。
/// TODO: 平台能力 / import 门禁与实例生命周期编排；底层 U-mode 机制位于 `sandbox`。
/// 当前不读 artifact、不声明实例，不降级成 KernelNative。
fn create_sandboxed_native(
    _name: &[u8],
    _args: &KcompCreateArgs,
) -> Result<ComponentId, ComponentLoadError> {
    Err(ComponentLoadError::SandboxUnsupported)
}

/// Isolated 装载的**前置门禁**（能力门禁之后、任何装载之前）。返回 artifact
/// 字节，调用方用同一份 blob 装载。
///
/// **import 白名单**（[`crate::component::isolated_load::SUPPORTED_IMPORTS`]）：
/// 诊断 / 只读查询、`kcore_panic_escape` 与内存 acquire/release 可解析；面外的 `kcore_*`
/// UNDEF 在这里拒绝（`-ENOTSUP`，绝不回退到裸 Core 地址）；其余具名 UNDEF 由
/// 按域装载的白名单检查拒绝（`isolated_load::place`，`-EINVAL`）。
///
/// 每个组件都从 artifact 重新放段（`isolated_lifecycle::create`）——没有
/// 跨域 / 同域 image 复用，因此不再需要域匹配 / 活跃实例门禁。
fn validate_isolated_load(name: &[u8]) -> Result<alloc::vec::Vec<u8>, ComponentLoadError> {
    let blob = read_artifact(name)?;
    check_isolated_imports(&blob)?;
    Ok(blob)
}

/// 扫描 ELF 符号表：任一具名 UNDEF 都必须命中 Isolated 的**唯一**支持白名单
/// （[`crate::component::isolated_load::import_supported`]）——不支持即显式拒绝，
/// 绝不回退到裸 Core 地址。
fn check_isolated_imports(blob: &[u8]) -> Result<(), ComponentLoadError> {
    let object =
        ElfObject::parse(blob).map_err(|error| ComponentLoadError::Loader(error.into()))?;
    let symbol_table = object
        .symbol_table_index()
        .map_err(|error| ComponentLoadError::Loader(error.into()))?;
    let count = object
        .symbol_count(symbol_table)
        .map_err(|error| ComponentLoadError::Loader(error.into()))?;
    for index in 0..count {
        let symbol = object
            .symbol(symbol_table, index)
            .map_err(|error| ComponentLoadError::Loader(error.into()))?;
        if symbol.shndx != 0 {
            continue; // 已定义（含 section / file 符号）。
        }
        let name = object
            .symbol_name(symbol_table, symbol)
            .map_err(|error| ComponentLoadError::Loader(error.into()))?;
        if !name.is_empty() && !crate::component::isolated_load::import_supported(name) {
            return Err(ComponentLoadError::IsolatedImportUnsupported);
        }
    }
    Ok(())
}

/// 从仓库读取 `<name>.kcomp` 的原始字节（`KernelNative` 与 `IsolatedNative`
/// 装载共用；两者对同一 artifact 的处理不同，但读取方式一致）。
pub(crate) fn read_artifact(name: &[u8]) -> Result<alloc::vec::Vec<u8>, ComponentLoadError> {
    let store = crate::component::store::get_component_store()
        .ok_or(ComponentLoadError::StoreNotMounted)?;
    let kname = [name, b".kcomp"].concat();
    let entries = store.list().map_err(|_| ComponentLoadError::ReadFailed)?;
    let entry = entries
        .iter()
        .find(|e| e.name.as_slice() == kname.as_slice())
        .ok_or(ComponentLoadError::NotFound)?;
    let mut blob = alloc::vec::Vec::new();
    blob.try_reserve_exact(entry.len)
        .map_err(|_| ComponentLoadError::Loader(LoaderError::OutOfMemory))?;
    blob.resize(entry.len, 0);
    store
        .read(&kname, &mut blob)
        .map_err(|_| ComponentLoadError::ReadFailed)?;
    Ok(blob)
}

// 这些用例需要 make test-host 准备的真实 `.kcomp` fixture（REAL_KPKG）；
// 真实工件由 make test-host 准备，test-fixtures 显式启用集成测试。
#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::component::ComponentState;
    use crate::test_support::{Rank, TestLock};

    /// 与 `store::tests` 同一份真实包（`manifest` + `kcomp_smoke.kcomp`）。
    const REAL_KPKG: &[u8] = include_bytes!(concat!(env!("KALEIDOS_TEST_FIXTURES"), "/init.kpkg"));

    /// 串行化本模块触碰全局真相（store / image / registry / endpoint / handle /
    /// HEAP）的测试；新增 load 相关用例都必须先拿这把锁。
    ///
    /// rank = LOAD（模块本地、最外层；见 [`crate::test_support`]）。
    static LOAD_TEST_LOCK: TestLock = TestLock::new(Rank::Load);

    /// 完整有序场景：NotFound → 成功到 `Ready` → 同名再创建得到**独立组件**
    /// （各自的 writable image）→ CURRENT 恢复。
    ///
    /// 为什么全放在一个测试里：store / registry 是进程级 `Once`，无法重置，
    /// 拆开会引入执行顺序依赖。host 边界（`arch::fake`）：`context_switch` 是
    /// no-op，组件入口体永不执行、trampoline 永不进入，`call_component_create` 恒
    /// 返回 `CallOutcome::Returned(0)`——因此能断言生命周期链走到 `Ready`，但不能
    /// 断言组件代码真实跑过（真实执行由 QEMU CoreTest 覆盖）。
    #[test]
    fn same_artifact_loads_produce_independent_components() {
        let _serial = LOAD_TEST_LOCK.lock();
        // `load_and_start` → `create_kernel_native` 经 `call_component_create`
        // 安装组件边界（Init guard）覆盖进程全局 `ACTIVE_GUARD`，必须持
        // BOUNDARY 锁（rank 0，先于 memory GUARD）。
        let _boundary = crate::component::containment::test_boundary_lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        // 本测试是全 crate 唯一挂载 store 的 host 路径（其余 `store::init` 调用点
        // 只有 boot 的 main32/main64），所以仓库内容必然是 REAL_KPKG——先挂载，
        // `kcomp_smoke.kcomp` 的查找才是确定性的。
        crate::component::store::init(REAL_KPKG);
        registry::init();
        endpoint::init();
        crate::resource::init();

        // Given：没有组件正在创建。
        assert_eq!(current_component(), None, "create 之外没有当前组件");

        // When：创建 store 中不存在的名字。
        // Then：NotFound（仓库已挂载，因此不是 StoreNotMounted）。
        assert_eq!(
            load_and_start(
                b"load_tests_missing_component",
                ExecutionDomain::KernelNative
            ),
            Err(ComponentLoadError::NotFound)
        );
        assert_eq!(current_component(), None, "失败路径不得残留 CURRENT");

        // When：创建真实 fixture 组件。
        // Then：生命周期提交到 Ready（Declared → Resolved → Starting → Ready）。
        let first = load_and_start(b"kcomp_smoke", ExecutionDomain::KernelNative)
            .expect("kcomp_smoke 必须创建成功");
        assert_eq!(current_component(), None, "create 之后 CURRENT 必须恢复");

        // When：同名 artifact 再次创建。
        // Then：**全新组件**，拥有自己独立的 loaded image backing（不共享
        // `.data` / `.bss`）——这是新模型的核心不变量。
        let second = load_and_start(b"kcomp_smoke", ExecutionDomain::KernelNative)
            .expect("同名再次创建必须成功");
        assert_ne!(first, second, "每次 instantiate 都是全新组件 id");
        let reg = registry::get_registry().lock();
        let a = reg.get(first).expect("first");
        let b = reg.get(second).expect("second");
        assert_eq!(a.state, ComponentState::Ready);
        assert_eq!(b.state, ComponentState::Ready);
        assert_eq!(a.name, b"kcomp_smoke");
        assert_eq!(b.name, b"kcomp_smoke");
        assert_ne!(
            a.loaded.memory.as_ref().map(|m| m.region().base),
            b.loaded.memory.as_ref().map(|m| m.region().base),
            "两次 instantiate 必须得到独立的常驻 backing"
        );
        let (a_base, a_end) = (a.loaded.base, a.loaded.base + a.loaded.text_size);
        let (b_base, b_end) = (b.loaded.base, b.loaded.base + b.loaded.text_size);
        assert!(
            a_end <= b_base || b_end <= a_base,
            "两次 instantiate 的镜像区间不得重叠：a=[{a_base:#x},{a_end:#x}) b=[{b_base:#x},{b_end:#x})"
        );

        // 未覆盖分支（host 不可确定性到达，不伪造）：
        // - StoreNotMounted：store 挂载后无法卸载（Once）；
        // - ReadFailed / Loader：REAL_KPKG 的条目与 ELF 都合法；
        // - ResolveFailed / StartFailed：无 requires，且声明成功后的状态机边都由
        //   本路径按序驱动，不可能被拒绝；
        // - CreateFailed / CreatePanicked / EndpointCommitFailed：入口体在 fake
        //   context backend 下不执行（恒 Returned(0)），无法产生非零返回、panic
        //   或 pending publication——真实执行 / 失败路径由 QEMU CoreTest 覆盖。
    }

    /// `with_current` 的作用域 / 恢复语义（生命周期接线用的身份纪律）：
    /// 嵌套调用保存 / 恢复外层身份，最外层返回后回到进入前的值。
    #[test]
    fn with_current_scopes_and_restores_the_creating_identity() {
        let _serial = LOAD_TEST_LOCK.lock();
        let before = current_component();
        let first = ComponentId::from_raw(0x51D1);
        let second = ComponentId::from_raw(0x51D2);

        with_current(first, || {
            assert_eq!(current_component(), Some(first));
            with_current(second, || {
                assert_eq!(current_component(), Some(second));
            });
            assert_eq!(current_component(), Some(first), "嵌套返回后恢复外层身份");
        });
        assert_eq!(current_component(), before, "最外层返回后恢复进入前的值");
    }

    /// Sandbox 执行器未实现，装载前返回能力缺失，不静默降级成 native。
    ///
    /// 独立于 store：分派发生在装载之前，因此本用例不需要挂载仓库。仍取
    /// LOAD_TEST_LOCK 与上面的全局真相用例串行。
    #[test]
    fn sandboxed_deployment_is_an_unimplemented_placeholder() {
        let _serial = LOAD_TEST_LOCK.lock();
        let before = current_component();
        let result = load_and_start(b"not_an_artifact", ExecutionDomain::SandboxedNative);
        assert_eq!(result, Err(ComponentLoadError::SandboxUnsupported));
        assert_eq!(result.unwrap_err().abi_status(), Errno::ENOTSUP.code());
        assert_eq!(current_component(), before);
    }

    // -- Isolated 部署的显式拒绝包络 --------------------------------------------

    /// 真实 `.kcomp` fixture（与 loader 用例同一份构建产物）。
    /// 明确不在 Isolated 支持面内的 import（`kcore_heap_alloc`）。
    const UNSUPPORTED_KCOMP: &[u8] = include_bytes!(concat!(
        env!("KALEIDOS_TEST_FIXTURES"),
        "/kcomp_isolated_unsupported.kcomp"
    ));

    /// **平台能力拒绝**：请求 `IsolatedNative` 而当前构建没有私有 AS backend
    /// （host 构建正是如此）→ `IsolationUnsupported`（`-ENOTSUP`）。
    ///
    /// 关键：`AddressSpaceBackend` trait 可用 ≠ 有隔离能力；拒绝发生在**读取
    /// 仓库与装载之前**，所以本用例不需要挂载 store。
    #[test]
    fn isolated_deployment_without_private_address_space_is_rejected() {
        let _serial = LOAD_TEST_LOCK.lock();

        // When：一个存在于仓库、但从未被读取过的名字。
        // Then：能力门禁先拒绝（不是 NotFound / StoreNotMounted，也不是静默 native）。
        assert_eq!(
            load_and_start(
                b"isolated_capability_probe",
                ExecutionDomain::IsolatedNative
            ),
            Err(ComponentLoadError::IsolationUnsupported)
        );
        assert_eq!(current_component(), None, "拒绝路径不得残留 CURRENT");
    }

    /// **import 支持面拒绝**：真实组件里不在 `SUPPORTED_IMPORTS` 内的 UNDEF
    /// 在装载前被拒绝——绝不回退到 KernelNative 的裸 Core 函数地址。
    #[test]
    fn isolated_load_rejects_unsupported_imports_before_loading() {
        assert_eq!(
            check_isolated_imports(UNSUPPORTED_KCOMP),
            Err(ComponentLoadError::IsolatedImportUnsupported),
            "kcore_heap_alloc 必须被 Isolated 装载拒绝"
        );
    }

    /// 唯一支持面由 isolated_load 定义；面外（调度 /
    /// 设备 / KernelNative 共享堆后端）一律不冒充"支持的 import"；空名（ELF NULL
    /// 符号）不算 import。
    #[test]
    fn isolated_import_filter_is_the_single_supported_surface() {
        use crate::component::isolated_load::import_supported;
        assert!(import_supported(b"kcore_log_line"));
        assert!(import_supported(b"kcore_now"));
        assert!(import_supported(b"kcore_panic_escape"));
        assert!(import_supported(b"kcore_memory_acquire"));
        assert!(import_supported(b"kcore_memory_release"));
        assert!(!import_supported(b"kcore_heap_alloc"));
        assert!(!import_supported(b"kcore_heap_dealloc"));
        assert!(!import_supported(b"kcore_device_claim"));
        assert!(!import_supported(b"kcore_"));
        assert!(!import_supported(b"kcomp_instance_create"));
        assert!(!import_supported(b"memcpy"));
        assert!(!import_supported(b""));
    }
}
