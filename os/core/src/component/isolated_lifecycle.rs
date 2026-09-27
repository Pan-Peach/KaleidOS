//! Isolated 域**实例生命周期**：私有 AS 切换 + 按域装载 + Core 实例状态机的接线。
//!
//! ```text
//! create_isolated_native(name, args)                    （load.rs 的门禁之后）
//!   ├─ isolated_load::place(blob)      ← 每次 instantiate 都重新按域放段
//!   │     └─ Core 验证：段 / 权限 / 入口 / abi（`isolated_load`）
//!   ├─ registry.declare(name, loaded, IsolatedNative) → create_isolated_address_space_for(id)
//!   ├─ map_mappings(段) + map_instance_windows()（组件栈 + 实例窗口）
//!   ├─ resolve → begin_start；写 create args / out_state / runtime slot 进窗口
//!   ├─ isolated::prepare(...)（Core 再验证入口 / 栈；共享 Core 映射已落）
//!   └─ isolated::enter(...) → Returned(0) → 提交 pending → Ready
//!
//! destroy（`exit.rs` 按 execution_domain 分派）
//!   └─ prepare(handle, record.loaded.destroy, ...) → enter → Returned(0) → retire(handle)
//!      → `complete_stop` 提交 Stopped
//!
//! restart：前一个组件 Failed / Stopped（tombstone）后，同名 create 从 artifact
//! **重新 instantiate**——全新 ComponentId / 全新私有 AS / 全新 backing / 新窗口 /
//! 新 runtime slot，且 writable image state 回到 artifact 初始状态。同一 artifact
//! 可以并发存在多个组件（各自私有 AS + backing）。
//!
//! service dispatch（KernelNative caller → Isolated provider）
//!   └─ dispatch_service(...)：容量校验（超长 `-EMSGSIZE`，绝不截断）→ 帧拷进
//!      **邮箱**（Core backing，实例域 VA）→ `with_isolated_service_boundary`
//!      包住 `enter` → Returned 时 output 拷回 caller；Faulted 时 fail_provider
//!      （Failed + AS 退役 + 窗口归还）→ EIO
//! ```
//!
//! # Core 验证 vs 组件提议
//!
//! - **Core 验证**：段规划 / 权限 / 入口 / abi（`isolated_load`）、私有 AS 能力、
//!   生命周期状态机、入口落在可执行映射、栈被可写映射覆盖
//!   （`isolated::prepare`）、`out_state` 由 Core 从**自己的视图**读回。
//! - **组件提议**：`kcomp_instance_create` 返回的 opaque state（Core 只存）与其
//!   内部行为；`kcomp_instance_destroy` 自行收尾。
//!
//! # 内存路径：窄 import 面 + Core 预置窗口
//!
//! Isolated 组件保持 S-mode，Core 代码 / 栈 / 全局状态在每个 Isolated AS 里
//! same VA → same PA，因此 **Isolated → Core 是普通直接调用**（`satp` 不切换）；
//! 装载前的 import 白名单只放诊断 / 只读查询与 `kcore_panic_escape`
//! （[`isolated_load::SUPPORTED_IMPORTS`]），其余具名 UNDEF 显式拒绝。
//! 跨域 service 调用仍由 **Core 主动发起**，provider 不需要回调 Core。create 的
//! 交付面是 Core 预置的**实例内存窗口**（[`ISOLATED_WINDOW_BASE`]：Core backing、
//! 零初始化、只映射在该实例私有 AS），交付 create args / out_state / runtime
//! context；窗口表示是**实例内 VA**（归属由该实例页表承载，Core 不另立账本），
//! 同一 VA 在别的实例 AS 里没有任何映射。service 邮箱
//! （[`ISOLATED_MAILBOX_BASE`]）同一 backing 纪律：扁平调用帧只经这里过边界
//! （拷贝，绝不共享），邮箱只在该实例私有 AS 里可达。
//!
//! 窗口布局（**Core 内部**；组件只用 Core 经 `a0` .. `a3` / `tp` 交给它的实例内
//! VA，不需要知道偏移）：
//!
//! ```text
//! +0    KcompCreateArgs（config_abi / config / config_len）
//! +32   out_state 槽（usize；create 返回后由 Core 读回）
//! +64   runtime context block（`tp` 指向这里；Core 从不解释其内容）
//! +128  config 负载拷贝（≤ WINDOW_CONFIG_MAX 字节）
//! +384  本窗口的**域视图编码**（`kcore_memory_view`：kind = LOCAL_VA、
//!       base = 本实例窗口 VA、len = 窗口长度）
//! ```
//!
//! 邮箱布局（**Core 内部**）见 [`isolated_mailbox`]：描述符 + args / input /
//! output 三个固定容量区。
//!
//! # 失败 / 重启矩阵
//!
//! | 阶段 | 终态 | AS | Core 预置窗口 | slot | caller 得到 |
//! |---|---|---|---|---|---|
//! | 放段 / 门禁失败（声明之前） | 无实例 | 未创建 | 未创建 | 未安装 | 类型化装载错误 |
//! | create 入口返回非零 / config 拒绝 | `Failed` | 退役 | 归还 backing | 清除 | `CreateFailed` / `IsolatedConfigRejected` |
//! | create 入口故障（trap） | `Failed` | 退役 | 归还 backing | 清除 | `CreateFaulted` |
//! | service dispatch 故障 | `Failed` | 退役 | 归还 backing | 清除 | `CallError::ProviderFailed`（EIO） |
//! | destroy 入口故障 | `Failed` | 退役 | **保持驻留** | 清除 | `DestroyPanicked`（EIO） |
//! | 优雅 destroy 成功 | `Stopped` | 退役 | **保持驻留** | 清除 | `Ok` |
//!
//! 读法：**create / service 故障 = Core 中止实例**（预置机制一并归还，半成品不留）；
//! **destroy 路径 = 实例已走到生命尽头**（无论入口成功或故障都只退役 AS，窗口
//! backing 驻留——AS 退役后不可再进入，页表页无 teardown 接口）。任何终态之后：
//! endpoint 永久失效（`failure` 兜底），stale 调用在 Core 边界被 `resolve` 拒绝
//! （先于任何进入），同一 artifact 可**重新 instantiate**（全新组件）。
//!
//! # 明确不做（当前边界）
//!
//! Isolated provider **不能自己 publish endpoint**：import 白名单只放诊断 / 只读
//! 查询与 `kcore_panic_escape`，endpoint 真相仍由 Core 拥有；出站 Isolated caller
//! 继续显式拒绝。destroy 后不做物理回收（窗口 / backing 驻留）。ASID 恒 0 + 全量
//! `sfence.vma`；没有 U-mode / `ecall`。邮箱容量固定，没有共享内存、没有 per-call
//! 映射。**组件内 Rust `panic!` 经 `kcore_panic_escape` 逃逸**（跨 AS 现场 →
//! trampoline 交回挂起的 Core 调用者，`Outcome::Faulted` 由调用方边界清理）；
//! 组件没有其他 Core import 面，真正的强制边界仍是 U-mode（未实现）。
//!
//! # 诚实边界
//!
//! - **协作式、非对抗**：S-mode 组件与 Core 同特权级，可以直接改 `satp` / 自己的
//!   映射；本模块不声称对抗隔离（那是 U-mode / SandboxedNative，未实现）。
//! - **CPU isolation ≠ DMA isolation**：Isolated 实例 AS = 共享 Core 映射
//!   （same VA → same PA）+ 该实例自己的镜像 / 栈 / 窗口 / 邮箱；**不含**别的
//!   实例的私有映射（页表保证 A 看不到 B 的 backing）。Core 拥有的 DMA backing
//!   是否可被错误复用仍不声称 DMA 静默。
//! - **每个组件拥有私有 backing**：同 artifact 两次 instantiate 各自按域放段 /
//!   重定位到独立 backing（writable `.data` / `.bss` 回到 artifact 初始状态），
//!   再经 Core 预置窗口承载实例级交付。**不回收**旧组件的 backing（phase 1：
//!   物理驻留）——ownership 明确归属旧组件。

use crate::component::ComponentId;
use crate::component::call::CallError;
use crate::component::containment::{CallOutcome, KcompCreateArgs};
use crate::component::endpoint::EndpointId;
use crate::component::isolated_load;
use crate::component::isolated_mailbox;
use crate::component::load::ComponentLoadError;
use crate::errno::Errno;
use crate::generated::abi::{KCORE_MEMORY_VIEW_LOCAL_VA, KcompCallFrame, MemoryView};
use crate::memory;
use crate::memory::address_space::VirtualRange;
use crate::task::TaskId;

/// 组件栈（Core 预置 backing；只映射在该实例的私有 AS 里）。
///
/// 4 页 = 16 KiB：与 KernelNative 的组件边界栈（`containment` 的 32 KiB）同档，
/// 这里取一半——Isolated 的 create / destroy 是短同步调用，够用且省页面。
pub const ISOLATED_STACK_BASE: usize = 0x2100_0000;
pub const ISOLATED_STACK_SIZE: usize = 16 * 1024;

/// **实例内存窗口**（Core 预置 backing；只映射在该实例的私有 AS 里）。
///
/// 与镜像窗口（[`isolated_load::ISOLATED_IMAGE_WINDOW`]）相邻但不重叠：镜像窗口
/// 结束（开区间）= 栈基址；本窗口在栈之上。
pub const ISOLATED_WINDOW_BASE: usize = 0x2200_0000;
pub const ISOLATED_WINDOW_SIZE: usize = memory::ALLOC_GRANULE;

/// **服务调用邮箱**（Core 预置 backing；只映射在该实例的私有 AS 里）。
///
/// 跨 AS 的扁平调用帧只经这里过边界（见 [`isolated_mailbox`]）：caller 的
/// args / input 被**拷贝**进邮箱，provider 在实例域内读它们并把 output 写回邮箱，
/// Core 再把 output 拷回 caller 的缓冲。provider 看不到 caller 的帧 / 缓冲，也
/// 看不到任何 Core 内存；邮箱页与实例窗口相邻、互不重叠。
pub const ISOLATED_MAILBOX_BASE: usize = ISOLATED_WINDOW_BASE + ISOLATED_WINDOW_SIZE;
pub const ISOLATED_MAILBOX_SIZE: usize = memory::ALLOC_GRANULE;

/// `KcompCreateArgs` 在窗口里的偏移（Core 写；组件经 `a0` 读）。
pub const WINDOW_ARGS_OFF: usize = 0;
/// `out_state` 槽在窗口里的偏移（Core 清零；组件写；Core 从自己的视图读回）。
pub const WINDOW_OUT_STATE_OFF: usize = 32;
/// 每实例 runtime context block 的偏移（`tp` = 窗口基址 + 本偏移；Core 不解释）。
pub const WINDOW_RUNTIME_OFF: usize = 64;
/// config 负载拷贝在窗口里的偏移（`args.config` 指向这里）。
pub const WINDOW_CONFIG_OFF: usize = 128;
/// config 负载上限（窗口内固定区；超出显式拒绝，绝不截断）。
pub const WINDOW_CONFIG_MAX: usize = 256;
/// 本窗口的**域视图编码**（`kcore_memory_view`）在窗口里的偏移。
///
/// Core 预交付该实例的域视图：`kind = KCORE_MEMORY_VIEW_LOCAL_VA`、
/// `base = ISOLATED_WINDOW_BASE`、`len = ISOLATED_WINDOW_SIZE`。表示是**实例内
/// VA**（与域视图契约同形）；不会出现物理地址或 Core 私有 VA。
pub const WINDOW_VIEW_OFF: usize = 384;

/// 本实例窗口的**域视图编码**：Core 预交付给实例的那份 `kcore_memory_view`。
///
/// `kind = LOCAL_VA`（表示是实例内 VA）、`base/len` = 本实例窗口；绝不出现物理
/// 地址 / Core 私有 VA。它是 Core 预交付的记录；可调用的 `kcore_memory_acquire`
/// 面不存在。
pub fn window_view() -> MemoryView {
    MemoryView {
        kind: KCORE_MEMORY_VIEW_LOCAL_VA,
        reserved: 0,
        base: ISOLATED_WINDOW_BASE as u64,
        len: ISOLATED_WINDOW_SIZE as u64,
    }
}

/// 组件栈的已映射区间（`prepare` / teardown 的坐标）。
pub fn stack_range() -> VirtualRange {
    VirtualRange {
        base: ISOLATED_STACK_BASE,
        size: ISOLATED_STACK_SIZE,
    }
}

/// 实例窗口的已映射区间。
pub fn window_range() -> VirtualRange {
    VirtualRange {
        base: ISOLATED_WINDOW_BASE,
        size: ISOLATED_WINDOW_SIZE,
    }
}

/// 服务调用邮箱的已映射区间。
pub fn mailbox_range() -> VirtualRange {
    VirtualRange {
        base: ISOLATED_MAILBOX_BASE,
        size: ISOLATED_MAILBOX_SIZE,
    }
}

// 布局不变量：镜像窗口（开区间结束）== 栈基址；栈 / 窗口 / 邮箱不重叠；
// 邮箱布局放得进邮箱页。
const _: () = {
    let image_end =
        isolated_load::ISOLATED_IMAGE_WINDOW.base + isolated_load::ISOLATED_IMAGE_WINDOW.size;
    assert!(image_end == ISOLATED_STACK_BASE);
    assert!(ISOLATED_STACK_BASE + ISOLATED_STACK_SIZE <= ISOLATED_WINDOW_BASE);
    assert!(ISOLATED_WINDOW_BASE + ISOLATED_WINDOW_SIZE <= ISOLATED_MAILBOX_BASE);
    assert!(isolated_mailbox::MAILBOX_BYTES <= ISOLATED_MAILBOX_SIZE);
    assert!(WINDOW_OUT_STATE_OFF + core::mem::size_of::<usize>() <= WINDOW_RUNTIME_OFF);
    assert!(WINDOW_CONFIG_OFF + WINDOW_CONFIG_MAX <= WINDOW_VIEW_OFF);
    assert!(WINDOW_VIEW_OFF + core::mem::size_of::<MemoryView>() <= ISOLATED_WINDOW_SIZE);
};

#[cfg(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
mod imp {
    use super::*;
    use crate::component::endpoint::{self, ExecutionDomain};
    use crate::component::isolated::{self, IsolatedPrepareError, Outcome};
    use crate::component::isolated_load::IsolatedLoadError;
    use crate::component::load;
    use crate::component::{containment, failure, registry, runtime_slot};
    use crate::memory::address_space::{
        self, AddressSpaceHandle, MapError, Mapping, MappingPermission, PhysicalRange,
    };

    /// 创建并启动一个 Isolated 组件（`load.rs::create_isolated_native` 的实现）。
    ///
    /// 每次 instantiate 都从 artifact **重新按域放段 + 重定位**，得到这个组件
    /// 自己私有的 backing（不再有同域 image 复用）。任何一步失败都走"半成品不留"：
    /// 退役 AS + 归还 Core 预置窗口 backing + `Failed`（组件一旦声明就一定有终态）。
    pub(crate) fn create(
        name: &[u8],
        blob: &[u8],
        args: &KcompCreateArgs,
    ) -> Result<ComponentId, ComponentLoadError> {
        // (1) 按域放段：段 / 权限 / 入口 / kcomp_abi 全部 Core 验证。
        let placed = isolated_load::place(blob).map_err(map_placement_error)?;
        let mappings = placed.mappings();
        let create_entry = placed.create();

        // (2) 声明组件：它 1:1 拥有这次加载结果（lease 随 loaded 常驻）。
        let id = registry::get_registry()
            .lock()
            .declare(
                name,
                placed.into_loaded_component(),
                ExecutionDomain::IsolatedNative,
            )
            .map_err(|_| ComponentLoadError::DeclareFailed)?;

        // (3) 私有 AS：还没有 AS 就没有可清理的，直接 Failed。
        let handle = match address_space::create_isolated_address_space_for(id) {
            Ok(handle) => handle,
            Err(error) => {
                let error = map_space_error(error);
                failure::fail_component(id, error);
                return Err(error);
            }
        };
        // AS 记进实例真相：stop / 失败清理 / ArchTest 都从这里解析句柄。
        if registry::get_registry()
            .lock()
            .record_address_space(id, handle)
            .is_err()
        {
            return Err(fail_with_as(id, handle, ComponentLoadError::StartFailed));
        }

        // (4) 落镜像段 + Core 预置窗口（栈 / 实例窗口 / 邮箱）。
        if let Err(error) = isolated_load::map_mappings(handle, &mappings) {
            return Err(fail_with_as(id, handle, map_placement_error(error)));
        }
        if let Err(error) = map_instance_windows(handle) {
            return Err(fail_with_as(id, handle, error));
        }

        // (5) Declared → Resolved → Starting（create 执行期）。
        let started = {
            let mut reg = registry::get_registry().lock();
            reg.resolve(id).and_then(|()| reg.begin_start(id))
        };
        if started.is_err() {
            return Err(fail_with_as(id, handle, ComponentLoadError::StartFailed));
        }

        // (6) 窗口预置：args / out_state（组件只见实例内 VA）+ runtime slot。
        let window = window_range();
        let window_backing = match backing_of(handle, &window) {
            Some(backing) => backing,
            None => return Err(fail_with_as(id, handle, ComponentLoadError::StartFailed)),
        };
        if let Err(error) = write_create_args(window_backing, args) {
            return Err(fail_with_as(id, handle, error));
        }
        let slot = window.base + WINDOW_RUNTIME_OFF;
        runtime_slot::get_slots()
            .lock()
            .install(id, slot as *mut ());

        // (7) Core 验证入口 / 栈（持锁阶段，返回后不持锁）。先把普通 trap 路径
        //     的异常钩子接到 Core：**没有显式策略就是 Abandon**（组件身份本身
        //     不是可恢复的证明），create 里的故障因此收敛成 `Outcome::Faulted`
        //     → `Failed`，而不是把 Core 打 panic。
        isolated::install();
        let transition = match isolated::prepare(
            handle,
            create_entry,
            stack_range(),
            slot,
            true,
            isolated::EntryArgs::pair(
                window.base + WINDOW_ARGS_OFF,
                window.base + WINDOW_OUT_STATE_OFF,
            ),
        ) {
            Ok(transition) => transition,
            Err(error) => return Err(fail_with_as(id, handle, map_prepare_error(error))),
        };

        // (8) 进入：组件在私有 AS 里执行 `kcomp_instance_create(args, out_state)`。
        //     期间 CURRENT = 本实例（与 KernelNative create 同一身份纪律）。
        match load::with_current(id, || isolated::enter(transition)) {
            Outcome::Returned(0) => {
                // Core 从**自己的视图**读回组件写下的 opaque state（绝不把实例内
                // VA 当 Core 指针解引用）。
                let instance_state = read_out_state(window_backing) as *mut ();
                if registry::get_registry()
                    .lock()
                    .record_instance_state(id, instance_state)
                    .is_err()
                {
                    return Err(fail_with_as(id, handle, ComponentLoadError::StartFailed));
                }
                // create 成功：原子提交 pending endpoints（Isolated 当前没有 import
                // 面，因此不会真有 pending；路径与 KernelNative 保持一致）。
                let commit = {
                    let reg = registry::get_registry().lock();
                    endpoint::get_endpoints().lock().commit_pending(&reg, id)
                };
                if let Err(error) = commit {
                    return Err(fail_with_as(
                        id,
                        handle,
                        ComponentLoadError::EndpointCommitFailed(error),
                    ));
                }
                if registry::get_registry().lock().finish_start(id).is_err() {
                    return Err(fail_with_as(id, handle, ComponentLoadError::StartFailed));
                }
                Ok(id)
            }
            // create 失败 / 故障：组件未完整构造，Core **不调用 destroy**。
            Outcome::Returned(code) => Err(fail_with_as(
                id,
                handle,
                ComponentLoadError::CreateFailed(code as u32 as i32),
            )),
            Outcome::Faulted => Err(fail_with_as(id, handle, ComponentLoadError::CreateFaulted)),
        }
    }

    /// 在私有 AS 里执行 `kcomp_instance_destroy(state)` 并退役该 AS。
    ///
    /// 分类交给 `exit.rs` 的 `complete_stop`（与 KernelNative 同一套终态语义）：
    /// 返回非零 / panic 由那里提交 `Failed`；成功由那里提交 `Stopped`。
    ///
    /// **窗口语义**：destroy 路径（成功或入口故障）一律只退役 AS，Core 预置窗口
    /// 保持驻留（AS 退役后不可再进入，页表页无 teardown 接口）。
    /// 这与 create / service 故障路径（Core 中止实例、归还预置窗口）不同——见模块
    /// 文档的失败矩阵。
    pub(crate) fn destroy(id: ComponentId, entry: usize, state: *mut ()) -> CallOutcome {
        let handle = match registry::get_registry()
            .lock()
            .get(id)
            .and_then(|record| record.address_space)
        {
            Some(handle) => handle,
            None => {
                crate::log!(
                    "component",
                    "isolated destroy: instance {} has no address space",
                    id.raw()
                );
                return CallOutcome::Returned(Errno::EIO.code());
            }
        };
        let slot = window_range().base + WINDOW_RUNTIME_OFF;
        // 与 create 同一纪律：组件故障交给 Core 的窄分派（无策略 = Abandon）。
        isolated::install();
        let transition = match isolated::prepare(
            handle,
            entry,
            stack_range(),
            slot,
            true,
            isolated::EntryArgs::pair(state as usize, 0),
        ) {
            Ok(transition) => transition,
            Err(error) => {
                crate::log!(
                    "component",
                    "isolated destroy: prepare failed for instance {}: {:?}",
                    id.raw(),
                    error
                );
                let _ = address_space::retire(handle);
                return CallOutcome::Returned(Errno::EIO.code());
            }
        };
        let outcome = match load::with_current(id, || isolated::enter(transition)) {
            Outcome::Returned(0) => CallOutcome::Returned(0),
            Outcome::Returned(code) => CallOutcome::Returned(code as u32 as i32),
            // Core trap 路径判为不可恢复：按 destroy panic 同档（Failed + 不重试）。
            Outcome::Faulted => CallOutcome::Panicked,
        };
        // 实例已被请求停止：AS 不再可能被进入（复用 = 新建空间），一律退役。
        let _ = address_space::retire(handle);
        outcome
    }

    /// 跨域 service dispatch：KernelNative caller → Isolated provider。
    ///
    /// 顺序（**Core 验证 vs 组件提议**）：
    ///
    /// 1. **Core 验证帧**：结构 + 容量（[`isolated_mailbox::check_frame`]）——
    ///    超长显式拒绝（`-EMSGSIZE`），绝不截断；
    /// 2. **Core 拷贝**：caller 的 args / input → 邮箱（provider 域内 VA）、
    ///    output 区清零；
    /// 3. **Core 验证入口 / 栈**（[`isolated::prepare`]：入口必须落在可执行
    ///    映射、栈被单条 R|W 映射覆盖）；
    /// 4. **边界 + 进入**：[`containment::with_isolated_service_boundary`] 装上
    ///    provider principal / caller-task provenance / re-entry / 调度门禁，然后
    ///    [`isolated::enter`] 把组件切进它自己的 AS；组件故障由**普通** trap
    ///    路径收敛（无显式策略 = `Abandon`）；
    /// 5. **Core 拷回**：provider 写的 output 区 → caller 的 `output` 缓冲（长度 =
    ///    caller 声明的 `output_len`）；provider 返回值 = **方法状态**写
    ///    `*out_status`，传输保持 `Ok`。
    ///
    /// provider 故障（`Outcome::Faulted`）或 Core 无法准备切换：provider 逻辑死亡
    /// + AS 退役 + Core 预置窗口归还（与 create 失败同一套清理），caller 拿到
    /// [`CallError::ProviderFailed`]（EIO），**caller 的 task 存活且不变**。
    ///
    /// # 栈（为什么这里没有 per-call 新栈）
    ///
    /// provider 跑在该实例 **Core 预置的组件栈**（[`stack_range`]；Core-owned
    /// backing、只映射在该实例的私有 AS 里）上，与同域 service 边界的 per-call 栈
    /// 目的相同、手段不同：同域栈要解决"共享 AS 里不能踩 caller 的栈"，跨 AS 的
    /// 隔离由页表承担（provider 根本看不到 caller 的栈）。调用是**同步且串行**的
    /// ——单 CPU、`prepare` 的 re-entry 门禁、且 Isolated provider 没有出站调用
    /// import 面（不能嵌套回调自己）——因此复用实例栈安全；故障时实例被放弃、
    /// AS 退役，栈不再被进入。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch_service(
        provider: ComponentId,
        endpoint: EndpointId,
        caller_task: Option<TaskId>,
        dispatcher: usize,
        instance_state: *mut (),
        port: u32,
        method: u32,
        frame: &KcompCallFrame,
        out_status: *mut i32,
    ) -> Result<(), CallError> {
        // (1) 帧结构 + 容量：任何拷贝之前显式拒绝（绝不截断）。
        if let Err(error) = isolated_mailbox::check_frame(frame) {
            registry::get_registry().lock().finish_call(provider);
            return Err(map_mailbox_error(error));
        }
        // (2) 实例 AS 句柄 + 邮箱 backing 的 Core 视图（Ready 实例必有两者；
        //     缺失 = Core 不变式破坏，按 provider 失败收尾，绝不 panic）。
        let handle = registry::get_registry()
            .lock()
            .get(provider)
            .and_then(|record| record.address_space);
        let Some(handle) = handle else {
            return Err(fail_provider(provider, None));
        };
        let Some(backing) = backing_of(handle, &mailbox_range()) else {
            return Err(fail_provider(provider, Some(handle)));
        };
        // (3) 拷贝进邮箱：provider 看到的是**实例域内**的 VA（绝不共享 caller 地址）。
        let mailbox =
            match unsafe { isolated_mailbox::write_frame(backing, ISOLATED_MAILBOX_BASE, frame) } {
                Ok(mailbox) => mailbox,
                // `check_frame` 已通过：这里不可达（防御：按 provider 失败收尾）。
                Err(_) => return Err(fail_provider(provider, Some(handle))),
            };
        // (4) Core 验证入口 / 栈（锁内；返回后不持锁）。组件故障交给普通 trap
        //     路径的异常钩子：**没有显式策略就是 Abandon**。
        isolated::install();
        let transition = match isolated::prepare(
            handle,
            dispatcher,
            stack_range(),
            window_range().base + WINDOW_RUNTIME_OFF,
            // 与同域 service 边界同一纪律：调用期间不开中断（provider 不可抢占）。
            false,
            isolated::EntryArgs {
                a0: instance_state as usize,
                a1: port as usize,
                a2: method as usize,
                a3: mailbox.frame,
            },
        ) {
            Ok(transition) => transition,
            Err(_) => return Err(fail_provider(provider, Some(handle))),
        };
        // (5) 身份边界 + 进入：provider 在自己的私有 AS 里执行 dispatcher。
        let outcome =
            containment::with_isolated_service_boundary(provider, endpoint, caller_task, || {
                isolated::enter(transition)
            });
        match outcome {
            Outcome::Returned(status) => {
                // SAFETY: 邮箱 backing 仍驻留（本实例 Ready、未退役）；`output_len`
                // 已由 `check_frame` 限界，caller 缓冲由 ABI 契约保证可写。
                unsafe {
                    isolated_mailbox::read_output(backing, frame.output, frame.output_len);
                }
                registry::get_registry().lock().finish_call(provider);
                // SAFETY: `out_status` 由调用方保证可写（入口已校验非空）；
                // unaligned 写防未对齐 UB。
                unsafe { core::ptr::write_unaligned(out_status, status as u32 as i32) };
                Ok(())
            }
            Outcome::Faulted => Err(fail_provider(provider, Some(handle))),
        }
    }

    /// provider 在 service 边界内故障 / Core 无法准备切换：逻辑死亡 + AS 退役 +
    /// Core 预置窗口归还（与 create 失败同一套清理），归还 inflight。
    fn fail_provider(provider: ComponentId, handle: Option<AddressSpaceHandle>) -> CallError {
        if let Some(handle) = handle {
            release_instance_windows(handle);
            let _ = address_space::retire(handle);
        }
        crate::component::fail_component(provider, ComponentLoadError::ServiceFaulted);
        registry::get_registry().lock().finish_call(provider);
        CallError::ProviderFailed
    }

    /// 落一段 Core 预置窗口（栈 / 实例窗口）：分配 backing、零初始化、映射 RW。
    fn map_window(
        handle: AddressSpaceHandle,
        range: VirtualRange,
    ) -> Result<(), ComponentLoadError> {
        let lease =
            memory::alloc_region(range.size).map_err(|_| ComponentLoadError::StartFailed)?;
        let base = lease.base();
        let size = lease.size();
        if size != range.size {
            // 防御：buddy 对 2 的幂请求必须原样返回（lease 随 Drop 归还）。
            return Err(ComponentLoadError::StartFailed);
        }
        // 别名排除：Core 预置窗口是组件私有的——先摘掉所有活着的 Isolated root
        // 里该 extent 的 identity 别名，再映射进本实例 AS。
        crate::memory::kernel_mappings::publish_private_backing(
            crate::memory::address_space::PhysicalRange { base, size },
        )
        .map_err(map_space_error)?;
        // 首次交付零初始化（与 `kcore_memory_acquire` 同一契约）。
        // SAFETY: base/size 来自 alloc_region；v1 identity / low-alias 视图下
        // 物理地址可写（与 loader / isolated_load 的放段方式相同）。
        unsafe { core::ptr::write_bytes(base as *mut u8, 0, size) };
        let mapping = Mapping {
            virtual_range: range,
            physical_range: PhysicalRange { base, size },
            permission: MappingPermission::READ | MappingPermission::WRITE,
        };
        match address_space::map(handle, mapping) {
            Ok(()) => {
                // backing 归该实例的 AS（页表即记录）；显式 release 走 unmap + free。
                core::mem::forget(lease);
                Ok(())
            }
            // mapping 失败：lease 在这里 Drop → backing 归还，不留半套窗口。
            Err(error) => Err(map_space_error(error)),
        }
    }

    fn map_instance_windows(handle: AddressSpaceHandle) -> Result<(), ComponentLoadError> {
        map_window(handle, stack_range())?;
        map_window(handle, window_range())?;
        map_window(handle, mailbox_range())
    }

    /// 把 create args / config 负载 / out_state / 域视图写进窗口 backing。
    ///
    /// `args.config` 是 Core 指针（config 负载），Core 只搬运不解释；长度超过
    /// 窗口固定区或指针 / 长度不自洽时显式拒绝，绝不截断。
    fn write_create_args(backing: usize, args: &KcompCreateArgs) -> Result<(), ComponentLoadError> {
        let len = usize::try_from(args.config_len)
            .map_err(|_| ComponentLoadError::IsolatedConfigRejected)?;
        if len > WINDOW_CONFIG_MAX || (len > 0 && args.config.is_null()) {
            return Err(ComponentLoadError::IsolatedConfigRejected);
        }
        let config_va = if len == 0 {
            core::ptr::null()
        } else {
            // config 指针指向窗口内的拷贝（实例内 VA；组件只经这个 VA 读）。
            (ISOLATED_WINDOW_BASE + WINDOW_CONFIG_OFF) as *const ()
        };
        let ctor = KcompCreateArgs {
            config_abi: args.config_abi,
            config: config_va,
            config_len: len,
        };
        // 本实例的域视图编码（表示 = 实例内 VA；绝不出现物理地址 / Core 私有 VA）。
        let view = window_view();
        // SAFETY: backing 是本实例窗口 backing 的 Core 视图（页对齐、独占）；三个
        // 偏移都在窗口内、对齐满足 `KcompCreateArgs` / `usize`；config 负载按
        // `len ≤ WINDOW_CONFIG_MAX` 已校验，拷贝不越界。
        unsafe {
            let dst = backing as *mut u8;
            core::ptr::write(dst.add(WINDOW_ARGS_OFF).cast::<KcompCreateArgs>(), ctor);
            core::ptr::write(dst.add(WINDOW_OUT_STATE_OFF).cast::<usize>(), 0);
            core::ptr::write(dst.add(WINDOW_VIEW_OFF).cast::<MemoryView>(), view);
            if len > 0 {
                core::ptr::copy_nonoverlapping(
                    args.config.cast::<u8>(),
                    dst.add(WINDOW_CONFIG_OFF),
                    len,
                );
            }
        }
        Ok(())
    }

    /// 读回组件写下的 `out_state`（从窗口 backing 的 Core 视图）。
    fn read_out_state(backing: usize) -> usize {
        // SAFETY: 同 `write_create_args`：backing 是本实例窗口的 Core 视图，
        // +WINDOW_OUT_STATE_OFF 对齐且可读。
        unsafe { core::ptr::read((backing + WINDOW_OUT_STATE_OFF) as *const usize) }
    }

    /// 窗口 backing 的 Core 视图（映射真相在实例 AS 里，Core 只读回自己的副本）。
    fn backing_of(handle: AddressSpaceHandle, range: &VirtualRange) -> Option<usize> {
        match address_space::mapping_exact(handle, range) {
            Ok(Some(mapping)) => Some(mapping.physical_range.base),
            _ => None,
        }
    }

    /// create 失败：退役 AS + 归还 Core 预置窗口 + 提交 `Failed`（半成品不留）。
    fn fail_with_as(
        id: ComponentId,
        handle: AddressSpaceHandle,
        error: ComponentLoadError,
    ) -> ComponentLoadError {
        release_instance_windows(handle);
        let _ = address_space::retire(handle);
        failure::fail_component(id, error);
        error
    }

    /// 解映射并归还 Core 预置窗口（best effort：状态提交不因回收失败而回滚）。
    fn release_instance_windows(handle: AddressSpaceHandle) {
        for range in [stack_range(), window_range(), mailbox_range()] {
            if let Ok(Some(mapping)) = address_space::mapping_exact(handle, &range) {
                let _ = address_space::unmap(handle, &range);
                let _ = memory::free_region_raw(
                    mapping.physical_range.base,
                    mapping.physical_range.size,
                );
            }
        }
    }

    fn map_placement_error(error: IsolatedLoadError) -> ComponentLoadError {
        match error {
            // 仓库读取失败的原因原样保留（不塌缩）。
            IsolatedLoadError::Artifact(error) => error,
            IsolatedLoadError::Loader(error) => ComponentLoadError::Loader(error),
            IsolatedLoadError::IsolationUnsupported => ComponentLoadError::IsolationUnsupported,
            _ => ComponentLoadError::IsolatedPlacementFailed,
        }
    }

    fn map_space_error(error: MapError) -> ComponentLoadError {
        match error {
            MapError::Unsupported => ComponentLoadError::IsolationUnsupported,
            _ => ComponentLoadError::StartFailed,
        }
    }

    fn map_prepare_error(error: IsolatedPrepareError) -> ComponentLoadError {
        match error {
            IsolatedPrepareError::Unsupported => ComponentLoadError::IsolationUnsupported,
            _ => ComponentLoadError::StartFailed,
        }
    }
}

/// 无真实私有 AS backend 的构建（host / NoMMU / 非 RISC-V target）：
/// 能力门禁在 `load.rs` 已先拒绝，这里保持接口形状并显式失败——**绝不**静默
/// 降级成"共享地址空间里跑一遍"。
#[cfg(not(all(
    feature = "vm-mmu",
    feature = "supervisor",
    any(target_arch = "riscv32", target_arch = "riscv64")
)))]
mod imp {
    use super::*;

    pub(crate) fn create(
        _name: &[u8],
        _blob: &[u8],
        _args: &KcompCreateArgs,
    ) -> Result<ComponentId, ComponentLoadError> {
        Err(ComponentLoadError::IsolationUnsupported)
    }

    pub(crate) fn destroy(_id: ComponentId, _entry: usize, _state: *mut ()) -> CallOutcome {
        // 生产不可达（没有 Isolated 实例能被创建）；显式失败而不是假装成功。
        CallOutcome::Returned(Errno::ENOTSUP.code())
    }

    /// 无私有 AS backend 的构建（host / NoMMU / 非 RISC-V）：没有 Isolated 实例
    /// 能被创建，service dispatch 显式拒绝——**绝不**在共享内核 AS 里替 Isolated
    /// provider 执行。帧结构 / 容量判据与真实路径**同一份**（host-testable），
    /// 因此 host 用例能锁定"超长帧先于能力拒绝"。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch_service(
        provider: ComponentId,
        _endpoint: EndpointId,
        _caller_task: Option<TaskId>,
        _dispatcher: usize,
        _instance_state: *mut (),
        _port: u32,
        _method: u32,
        frame: &KcompCallFrame,
        _out_status: *mut i32,
    ) -> Result<(), CallError> {
        if let Err(error) = isolated_mailbox::check_frame(frame) {
            crate::component::registry::get_registry()
                .lock()
                .finish_call(provider);
            return Err(map_mailbox_error(error));
        }
        crate::component::registry::get_registry()
            .lock()
            .finish_call(provider);
        Err(CallError::UnsupportedProviderDomain)
    }
}

pub(crate) use imp::{create, destroy, dispatch_service};

/// 邮箱拒绝 → 调用传输错误（两条实现共用，唯一映射点）。
fn map_mailbox_error(error: isolated_mailbox::MailboxError) -> CallError {
    match error {
        isolated_mailbox::MailboxError::InvalidFrame => CallError::InvalidFrame,
        isolated_mailbox::MailboxError::FrameTooLarge => CallError::FrameTooLarge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 窗口字段按序、不重叠、都在窗口内（偏移对 RV32 / RV64 是同一份）。
    #[test]
    fn window_fields_are_ordered_and_inside_the_window() {
        let fields = [
            (WINDOW_ARGS_OFF, core::mem::size_of::<KcompCreateArgs>()),
            (WINDOW_OUT_STATE_OFF, core::mem::size_of::<usize>()),
            (WINDOW_RUNTIME_OFF, 64),
            (WINDOW_CONFIG_OFF, WINDOW_CONFIG_MAX),
            (WINDOW_VIEW_OFF, core::mem::size_of::<MemoryView>()),
        ];
        for pair in fields.windows(2) {
            assert!(
                pair[0].0 + pair[0].1 <= pair[1].0,
                "window fields must not overlap"
            );
        }
        assert_eq!(fields[0].0, 0, "args 必须在窗口起点");
        let end = fields.last().map_or(0, |(offset, len)| offset + len);
        assert!(end <= ISOLATED_WINDOW_SIZE, "config 区必须落在窗口内");
    }

    /// Core 预交付的域视图编码：LOCAL_VA、base/len = 本实例窗口（表示是实例内 VA）。
    #[test]
    fn window_view_encodes_the_instance_local_window() {
        let view = window_view();
        assert_eq!(view.kind, KCORE_MEMORY_VIEW_LOCAL_VA);
        assert_eq!(view.reserved, 0);
        assert_eq!(view.base, ISOLATED_WINDOW_BASE as u64);
        assert_eq!(view.len, ISOLATED_WINDOW_SIZE as u64);
    }

    /// 实例窗口 / 栈 / 邮箱与镜像窗口不重叠：镜像窗口开区间结束 == 栈基址，
    /// 栈在窗口之下，邮箱在窗口之上。
    #[test]
    fn instance_ranges_do_not_overlap_the_image_window() {
        let ranges = [
            isolated_load::ISOLATED_IMAGE_WINDOW,
            stack_range(),
            window_range(),
            mailbox_range(),
        ];
        for pair in ranges.windows(2) {
            assert!(
                pair[0].base + pair[0].size <= pair[1].base,
                "instance ranges must not overlap"
            );
        }
        // 邮箱布局放得进邮箱页（`const _` 已在编译期钉住，这里再显式一次）。
        const { assert!(isolated_mailbox::MAILBOX_BYTES <= ISOLATED_MAILBOX_SIZE) };
        assert_eq!(
            ISOLATED_MAILBOX_BASE,
            ISOLATED_WINDOW_BASE + ISOLATED_WINDOW_SIZE
        );
    }

    /// 无私有 AS backend 的构建（host）上，创建入口显式拒绝——绝不静默降级。
    #[test]
    fn create_is_rejected_without_a_private_address_space_backend() {
        let args = KcompCreateArgs {
            config_abi: 0,
            config: core::ptr::null(),
            config_len: 0,
        };
        assert_eq!(
            create(b"isolated_lifecycle_host_probe", &[], &args),
            Err(ComponentLoadError::IsolationUnsupported)
        );
    }
}
