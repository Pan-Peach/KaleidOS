//! 组件实例创建语义入口（ComponentManager 教学版占位）：仓库读取 → image 复用或
//! loader 放段 → image 登记 → registry 声明实例 → resolve → begin_start（Starting）→
//! 调用 `kcomp_instance_create(args, &out_state)` → 记录 state → 原子提交 pending
//! endpoints → finish_start（Ready）。
//!
//! `monitor load <name>` 与组件 ABI `kcore_component_load` 都是这里的**薄 caller**——
//! 加载流程本身属于 Core（monitor 不是 ComponentManager）。完整依赖解析、
//! kpkg manifest requires、失败回滚留给真正的 ComponentManager 里程碑。
//!
//! # 一份 image，N 个实例（`docs/architecture/component-lifecycle.md` §2/§3）
//!
//! 同名 artifact 再次创建**复用已登记的 image**（新实例、新 `ComponentId`、新
//! state），不再拒绝；image 登记进 `component/image.rs` 的 image 表并 pinned 到重启。

use crate::component::containment::{self, CallOutcome, KcompCreateArgs};
use crate::component::elf::ElfObject;
use crate::component::endpoint::{self, EndpointError, ExecutionDomain};
use crate::component::image::{self, ComponentImageId};
use crate::component::loader::{self, LoaderError};
use crate::component::{ComponentId, failure, registry};
use crate::errno::Errno;
use crate::memory::address_space::{
    self, AddressSpaceHandle, MapError, Mapping, MappingPermission, PhysicalRange, VirtualRange,
};
use crate::task::TaskId;
use spin::Mutex;

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
    /// image 登记失败（名字过长 / image id 耗尽）。
    ImageFailed,
    /// registry 声明实例失败（id 耗尽）。
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
    /// 请求 `IsolatedNative` 部署，但当前平台 / profile **没有私有地址空间能力**
    /// （NoMMU 恒等 backend，或没有真实 backend）：`AddressSpaceBackend` 可用
    /// **不等于**有隔离能力 → `-ENOTSUP`，绝不把恒等映射当私有 AS 用。
    IsolationUnsupported,
    /// `IsolatedNative` 装载发现**未支持的 `kcore_*` import**：本阶段 Isolated
    /// 的 import 解析（Core gate trampoline）尚未实现，任何 `kcore_*` UNDEF 都
    /// 在装载**之前**拒绝——绝不回退到 KernelNative 的裸 Core 函数地址。
    IsolatedImportUnsupported,
    /// `IsolatedNative` 装载不得复用**已按 KernelNative 放段 / 重定位**的 image：
    /// 其 VA 布局与 import 目标（裸 Core 地址）都是共享内核 AS 的产物，复用等于
    /// 把裸 Core 地址带进 Isolated 域。按域装载落地前一律拒绝。
    IsolatedImageReuse,
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

/// 当前正在创建的实例（create 调用期间由 Core 记录）。
///
/// `kcore_endpoint_publish` 的 provider 以及锚点上
/// create 阶段的 task requester 从这里解析——组件不需要知道自己/别人的
/// ComponentId，Core 不信任组件自报的身份。普通任务的 requester 从
/// `TaskRecord.owner` 解析。嵌套创建（组件 create 里再创建别的组件）时保存/恢复。
static CURRENT: Mutex<Option<ComponentId>> = Mutex::new(None);

/// 取当前正在创建的实例；不在 create 调用内返回 None。
pub fn current_component() -> Option<ComponentId> {
    *CURRENT.lock()
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
/// 走 [`create_isolated_native`] 的**受限门禁**（能力 / import 包络 / image 复用
/// 任一不满足即 `-ENOTSUP`，不执行组件）；`SandboxedNative` 尚无占位实现
/// （`todo!()`）。任何域都**绝不静默降级成 native 跑**
/// （`docs/architecture/deployment.md` §2 ⑤/§10）。
///
/// 锁纪律：registry / image 锁只覆盖各自的查询与提交；`kcomp_instance_create`
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

    // 按执行域分派：每个域一个创建入口——接入新域 = 新增一个臂，而不是"放开一道
    // guard"（guard 会让"未实现域"与"已实现域"共用同一条装载路径，混淆真相）。
    match kind {
        ExecutionDomain::KernelNative => create_kernel_native(name, args),
        // Isolated 执行器尚未落地（assembly gateway / satp 切换是后续 increment）：
        // `create_isolated_native` 只做**受限门禁 + 私有 AS 准备**，不调用组件入口。
        ExecutionDomain::IsolatedNative => create_isolated_native(name, args),
        // TODO(human): Sandbox 执行器——U-mode + 私有 AS + ecall。
        ExecutionDomain::SandboxedNative => todo!("Sandbox 执行器未实现"),
    }
}

/// `KernelNative` 的创建路径（今天唯一有真实执行器的域）。
///
/// 生命周期：`Declared → resolve → Resolved → begin_start → Starting →
/// kcomp_instance_create → { failure → Failed | success → record state →
/// commit pending endpoints → Ready }`。同名 artifact 复用已登记的
/// image；不存在则先走 store → loader → image 登记。
fn create_kernel_native(
    name: &[u8],
    args: &KcompCreateArgs,
) -> Result<ComponentId, ComponentLoadError> {
    let image = get_or_load_image(name)?;
    let create_entry = image::get_images()
        .lock()
        .get(image)
        .map(|image| image.create)
        .ok_or(ComponentLoadError::ImageFailed)?;

    let id = {
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(image, ExecutionDomain::KernelNative)
            .map_err(|_| ComponentLoadError::DeclareFailed)?;
        reg.resolve(id)
            .map_err(|_| ComponentLoadError::ResolveFailed)?;
        // Resolved → Starting：`kcomp_instance_create` 执行期间 publish 只记录 pending。
        reg.begin_start(id)
            .map_err(|_| ComponentLoadError::StartFailed)?;
        id
    };

    // 入口调用：期间 CURRENT = 本实例（publish / task_create 的身份来源）。
    // Core 先把 out_state 置 NULL（无状态组件可成功写回 NULL）。
    let mut instance_state: *mut () = core::ptr::null_mut();
    let previous = *CURRENT.lock();
    *CURRENT.lock() = Some(id);
    let outcome = containment::call_component_create(create_entry, args, &mut instance_state);
    *CURRENT.lock() = previous;

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

/// `IsolatedNative` 的**受限装载门禁**（本函数**不执行任何组件**）。
///
/// 顺序（任一不满足即显式拒绝，绝不降级成 KernelNative）：
/// 1. **平台能力**：当前 profile 必须有私有地址空间 backend（NoMMU / 无后端 → 拒绝）；
/// 2. **image 复用 + import 包络**：不得复用 KernelNative 放段结果，不得含
///    未支持的 `kcore_*` import（见 [`validate_isolated_load`]）；
/// 3. 建立该实例的私有 AS 并只映射装载镜像；
/// 4. 声明实例 → `resolve` → `begin_start`——`kcomp_instance_create` **不调用**：
///    真正的 Isolated 入口执行需要 assembly gateway / satp 切换（后续 increment）。
fn create_isolated_native(
    name: &[u8],
    _args: &KcompCreateArgs,
) -> Result<ComponentId, ComponentLoadError> {
    // (1) 能力门禁：trait 可用 ≠ 隔离能力（NoMMU 也实现 AddressSpaceBackend）。
    if !address_space::isolation_capable() {
        return Err(ComponentLoadError::IsolationUnsupported);
    }

    // (2) 装载前置门禁；blob 只读一次，随后的装载用同一份字节。
    let blob = validate_isolated_load(name)?;
    let loaded = loader::load_component(&blob).map_err(ComponentLoadError::Loader)?;
    let image = image::get_images()
        .lock()
        .register(name, loaded)
        .map_err(|_| ComponentLoadError::ImageFailed)?;

    let id = registry::get_registry()
        .lock()
        .declare(image, ExecutionDomain::IsolatedNative)
        .map_err(|_| ComponentLoadError::DeclareFailed)?;

    // (3) 私有 AS：只映射该实例自己的装载镜像（不做 Core 段 / 堆 / MMIO 的全量映射）。
    let space = create_isolated_address_space(id, image)?;
    {
        let mut reg = registry::get_registry().lock();
        reg.record_address_space(id, space)
            .map_err(|_| ComponentLoadError::StartFailed)?;
    }

    // (4) 生命周期只推进到 `Starting`：没有执行器就不会有 Ready 实例。
    {
        let mut reg = registry::get_registry().lock();
        reg.resolve(id)
            .map_err(|_| ComponentLoadError::ResolveFailed)?;
        reg.begin_start(id)
            .map_err(|_| ComponentLoadError::StartFailed)?;
    }

    Ok(id)
}

/// Isolated 装载的**前置门禁**（能力门禁之后、任何装载之前）。返回 artifact
/// 字节，调用方用同一份 blob 装载（不重复读取）。
///
/// - **image 复用拒绝**：image 表按 artifact 名唯一，而今天登记的都是
///   KernelNative 放段 / 重定位结果（import = 裸 Core 函数地址，VA 按共享内核
///   AS 选定）。复用它们正是"静默降级"——按域装载落地前一律拒绝。
/// - **import 包络**：本阶段唯一受支持的 Isolated import 集合是**空集**
///   （`kcore_*` 尚无 per-domain gate 解析）。任何 `kcore_*` UNDEF 在装载前拒绝。
fn validate_isolated_load(name: &[u8]) -> Result<alloc::vec::Vec<u8>, ComponentLoadError> {
    if image::get_images().lock().find(name).is_some() {
        return Err(ComponentLoadError::IsolatedImageReuse);
    }
    let blob = read_artifact(name)?;
    check_isolated_imports(&blob)?;
    Ok(blob)
}

/// 扫描 ELF 符号表：任一 **UNDEF**（`shndx == 0`）的 `kcore_*` 符号都是未支持的
/// Isolated import（见 [`validate_isolated_load`]）。
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
        if is_kcore_import(name) {
            return Err(ComponentLoadError::IsolatedImportUnsupported);
        }
    }
    Ok(())
}

/// 该符号名是否属于 `kcore_*` 导出面（即 loader 在 KernelNative 下会重定位到
/// **裸 Core 地址**的那一类）。非 `kcore_*` 的 UNDEF 不在本包络内：它们本来就
/// 无法解析，由 loader 自己以 `UnresolvedSymbol` 拒绝。
fn is_kcore_import(name: &[u8]) -> bool {
    name.starts_with(b"kcore_")
}

fn create_isolated_address_space(
    owner: ComponentId,
    image: ComponentImageId,
) -> Result<AddressSpaceHandle, ComponentLoadError> {
    let handle = address_space::create_address_space_for(owner).map_err(|error| match error {
        // 能力门禁的兜底：backend 自己声明没有私有 AS → 同样是"域不支持"，
        // 不是 I/O 失败。
        MapError::Unsupported => ComponentLoadError::IsolationUnsupported,
        _ => ComponentLoadError::StartFailed,
    })?;

    let mapping = {
        let images = image::get_images().lock();
        let comp_image = images.get(image).ok_or(ComponentLoadError::ImageFailed)?;
        let backing = comp_image.memory.region();
        let mapped_size = crate::memory::align_up_page(comp_image.text_size);

        if mapped_size == 0 || mapped_size > backing.size {
            return Err(ComponentLoadError::StartFailed);
        }

        Mapping {
            virtual_range: VirtualRange {
                base: comp_image.base,
                size: mapped_size,
            },
            physical_range: PhysicalRange {
                base: backing.base,
                size: mapped_size,
            },
            permission: MappingPermission::READ | MappingPermission::EXECUTE,
        }
    };
    address_space::map(handle, mapping).map_err(|_| ComponentLoadError::StartFailed)?;

    Ok(handle)
}

///
/// TODO(human): 按域装载。image 表按 **artifact 名**索引、单一 load base、import
/// 只重定位一次（deployment.md §6.2/§7.2）——同名 Native 镜像不能直接拿来在
/// Isolated 域执行（VA 布局与 import 目标都不同，需按域重新放段 + 重解析 import）。
/// 接入 Isolated loader 后，这里需按 `(name, kind)` 索引（或按 kind 分派），
/// monitor 的 "already loaded" 检查同样要按请求的 kind 判定。当前非 KernelNative
/// 已在 create_component 入口被拒绝，故这里只处理 KernelNative。
fn get_or_load_image(name: &[u8]) -> Result<ComponentImageId, ComponentLoadError> {
    if let Some(id) = image::get_images().lock().find(name) {
        return Ok(id);
    }

    let blob = read_artifact(name)?;
    let comp = loader::load_component(&blob).map_err(ComponentLoadError::Loader)?;
    image::get_images()
        .lock()
        .register(name, comp)
        .map_err(|_| ComponentLoadError::ImageFailed)
}

/// 从仓库读取 `<name>.kcomp` 的原始字节（`KernelNative` 与 `IsolatedNative`
/// 装载共用；两者对同一 artifact 的处理不同，但读取方式一致）。
fn read_artifact(name: &[u8]) -> Result<alloc::vec::Vec<u8>, ComponentLoadError> {
    let store = crate::component::store::get_component_store()
        .ok_or(ComponentLoadError::StoreNotMounted)?;
    let kname = [name, b".kcomp"].concat();
    let entries = store.list().map_err(|_| ComponentLoadError::ReadFailed)?;
    let entry = entries
        .iter()
        .find(|e| e.name.as_slice() == kname.as_slice())
        .ok_or(ComponentLoadError::NotFound)?;
    let mut blob = alloc::vec![0u8; entry.len];
    store
        .read(&kname, &mut blob)
        .map_err(|_| ComponentLoadError::ReadFailed)?;
    Ok(blob)
}

// 这些用例需要 os/core/build.rs 生成的真实 `.kcomp` fixture（REAL_KPKG）；
// KALEIDOS_CORE_ONLY 下跳过组件构建，故用 `no_kcomp` 门控。
#[cfg(all(test, not(no_kcomp)))]
mod tests {
    use super::*;
    use crate::component::ComponentState;
    use crate::test_support::{Rank, TestLock};

    /// 与 `store::tests` 同一份真实包（`manifest` + `kcomp_smoke.kcomp`）。
    const REAL_KPKG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.kpkg"));

    /// 串行化本模块触碰全局真相（store / image / registry / endpoint / handle /
    /// HEAP）的测试；将来新增 load 相关用例都必须先拿这把锁。
    ///
    /// rank = LOAD（模块本地、最外层；见 [`crate::test_support`]）。
    static LOAD_TEST_LOCK: TestLock = TestLock::new(Rank::Load);

    /// 完整有序场景：NotFound → 成功到 `Ready` → 同名再创建得到**共享 image 的
    /// 新实例** → CURRENT 恢复。
    ///
    /// 为什么全放在一个测试里：store / image / registry 是进程级 `Once`，无法重置，
    /// 拆开会引入执行顺序依赖。host 边界（`arch::fake`）：`context_switch` 是
    /// no-op，组件入口体永不执行、trampoline 永不进入，`call_component_create` 恒
    /// 返回 `CallOutcome::Returned(0)`——因此能断言生命周期链走到 `Ready`，但不能
    /// 断言组件代码真实跑过（真实执行由 QEMU CoreTest 覆盖）。
    #[test]
    fn load_and_start_shares_one_image_across_instances() {
        let _serial = LOAD_TEST_LOCK.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        // 本测试是全 crate 唯一挂载 store 的 host 路径（其余 `store::init` 调用点
        // 只有 boot 的 main32/main64），所以仓库内容必然是 REAL_KPKG——先挂载，
        // `kcomp_smoke.kcomp` 的查找才是确定性的。
        crate::component::store::init(REAL_KPKG);
        image::init();
        registry::init();
        endpoint::init();
        crate::resource::init();

        // Given：没有实例正在创建。
        assert_eq!(current_component(), None, "create 之外没有当前实例");

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
        assert_eq!(
            registry::get_registry().lock().get(first).map(|r| r.state),
            Some(ComponentState::Ready)
        );
        let first_image = registry::get_registry()
            .lock()
            .get(first)
            .expect("first instance")
            .image;
        assert_eq!(current_component(), None, "create 之后 CURRENT 必须恢复");

        // When：同名 artifact 再次创建（一份 image、两个实例）。
        // Then：新实例、新 id、共享同一 image，两者都 Ready。
        let second = load_and_start(b"kcomp_smoke", ExecutionDomain::KernelNative)
            .expect("同名再次创建必须成功");
        assert_ne!(first, second, "每次创建都是全新实例 id");
        let reg = registry::get_registry().lock();
        assert_eq!(
            reg.get(second).unwrap().image,
            first_image,
            "共享同一 image"
        );
        assert_eq!(reg.get(first).unwrap().state, ComponentState::Ready);
        assert_eq!(reg.get(second).unwrap().state, ComponentState::Ready);
        // image 表按名字唯一：`find` 仍解析到同一份（全局表里可能有其它测试的 image）。
        assert_eq!(
            image::get_images().lock().find(b"kcomp_smoke"),
            Some(first_image)
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

    /// Sandbox 执行器未实现（`todo!()` 占位），不是静默降级成 native。
    ///
    /// 独立于 store / image：分派发生在 `get_or_load_image` 之前，因此本用例不需要
    /// 挂载仓库。仍取 LOAD_TEST_LOCK 与上面的全局真相用例串行。
    #[test]
    #[should_panic(expected = "Sandbox 执行器未实现")]
    fn sandboxed_deployment_is_an_unimplemented_placeholder() {
        let _serial = LOAD_TEST_LOCK.lock();
        let _ = load_and_start(b"kcomp_smoke", ExecutionDomain::SandboxedNative);
    }

    // -- Isolated 部署的显式拒绝包络（increment 1）-----------------------------

    /// 真实 `.kcomp` fixture（与 loader 用例同一份构建产物）。
    const SMOKE_KCOMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kcomp_smoke.kcomp"));

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

    /// **import 包络拒绝**：真实组件的 `kcore_*` UNDEF 在装载前被拒绝——绝不
    /// 回退到 KernelNative 的裸 Core 函数地址。
    #[test]
    fn isolated_load_rejects_kcore_imports_before_loading() {
        assert_eq!(
            check_isolated_imports(SMOKE_KCOMP),
            Err(ComponentLoadError::IsolatedImportUnsupported),
            "kcomp_smoke 的 kcore_* import 必须被 Isolated 装载拒绝"
        );
    }

    /// `is_kcore_import` 只认 `kcore_*` 导出面：空名 / 其它 UNDEF 由 loader
    /// 自己的 `UnresolvedSymbol` 处理，不冒充"支持的 import"。
    #[test]
    fn isolated_import_classifier_matches_the_kcore_export_surface() {
        assert!(is_kcore_import(b"kcore_log_line"));
        assert!(is_kcore_import(b"kcore_"));
        assert!(!is_kcore_import(b""));
        assert!(!is_kcore_import(b"kcomp_instance_create"));
        assert!(!is_kcore_import(b"memcpy"));
    }

    /// **image 复用拒绝**：Isolated 请求不得复用已登记的 KernelNative image
    /// （其 import 目标是裸 Core 地址、VA 按共享内核 AS 选定）。
    #[test]
    fn isolated_load_rejects_reusing_an_existing_image() {
        let _serial = LOAD_TEST_LOCK.lock();
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();
        image::init();

        // Given：一份按 KernelNative 放段 / 重定位的已登记 image（测试替身）。
        image::test_support::register_test_image(b"isolated_reuse_probe", 0);

        // When / Then：Isolated 装载在 image 复用检查处拒绝，不读仓库、不装载。
        assert_eq!(
            validate_isolated_load(b"isolated_reuse_probe"),
            Err(ComponentLoadError::IsolatedImageReuse)
        );
    }
}
