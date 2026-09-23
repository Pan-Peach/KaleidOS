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
use crate::component::endpoint::{self, EndpointError, ExecutionDomain};
use crate::component::image::{self, ComponentImageId};
use crate::component::loader::{self, LoaderError};
use crate::component::{ComponentId, failure, registry};
use crate::errno::Errno;
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
/// `KernelNative` 走 [`create_kernel_native`]（现有完整创建链）；`IsolatedNative` /
/// `SandboxedNative` 尚无执行器，是**显式占位**（`todo!()`），绝不静默降级成 native
/// 跑（`docs/architecture/deployment.md` §2 ⑤/§10）。
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
        // TODO(human): Isolated 执行器——按域放段 + 按域 import 解析 + 私有 AS
        // 入口（deployment.md §6.2/§7.1）；配合 `get_or_load_image` 的按域装载。
        ExecutionDomain::IsolatedNative => todo!("Isolated 执行器未实现"),
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
    //
    // TODO(human): 执行入口按域分派。当前 `call_component_create` 只切独立栈，没有
    // 地址空间切换（deployment.md §7.1：`activate()` 无人调用、无 satp 切换、无
    // U-mode）。Isolated 实例必须等域运行环境 + 按域装载 + 按域入口都就绪后，才越过
    // begin_start 执行组件代码——在那之前，`create_component` 的按域分派对
    // Isolated/Sandbox 是 `todo!()` 占位，根本不会走到这里。
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

/// 取同名已登记的 image；没有就用 store + loader 加载一份并登记。
///
/// image 登记是 pinned-until-reboot：第二、第三个实例只会复用，不会重新加载
/// （即使并发加载在 register 处撞上，同名也只会保留第一份）。
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

    let comp = loader::load_component(&blob).map_err(ComponentLoadError::Loader)?;
    image::get_images()
        .lock()
        .register(name, comp)
        .map_err(|_| ComponentLoadError::ImageFailed)
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

    /// 未实现的执行域是**显式占位**（`todo!()`），不是静默降级成 native。
    ///
    /// 独立于 store / image：分派发生在 `get_or_load_image` 之前，因此本用例不需要
    /// 挂载仓库。仍取 LOAD_TEST_LOCK 与上面的全局真相用例串行。
    /// 实现 Isolated 执行器后本用例会失败——届时改成真实路径的断言。
    #[test]
    #[should_panic(expected = "Isolated 执行器未实现")]
    fn isolated_deployment_is_an_unimplemented_placeholder() {
        let _serial = LOAD_TEST_LOCK.lock();
        let _ = load_and_start(b"kcomp_smoke", ExecutionDomain::IsolatedNative);
    }

    /// 同上：Sandbox 执行器未实现（`todo!()` 占位）。
    #[test]
    #[should_panic(expected = "Sandbox 执行器未实现")]
    fn sandboxed_deployment_is_an_unimplemented_placeholder() {
        let _serial = LOAD_TEST_LOCK.lock();
        let _ = load_and_start(b"kcomp_smoke", ExecutionDomain::SandboxedNative);
    }
}
