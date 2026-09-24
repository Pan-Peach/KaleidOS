//! Isolated 域**实例生命周期**（increment 5）：把 increment 3 的私有 AS 切换
//! gateway、increment 4 的按域装载与 Core 的实例状态机接起来，让
//! `create_isolated_native` 真正创建、启动、销毁一个 Isolated 实例。
//!
//! ```text
//! create_isolated_native(name, args)                    （load.rs 的门禁之后）
//!   ├─ isolated_load::place(blob)           ← 按域放段（Core 验证：段 / 权限 / 入口 / abi）
//!   ├─ image 表登记（pinned-until-reboot）   ← lease 归 image
//!   ├─ registry.declare(image, IsolatedNative)
//!   ├─ create_address_space_for(id)         ← 私有 AS（Core 拥有）
//!   ├─ isolated_load::map_mappings(段)      ← 页表 = 本实例的归属记录
//!   ├─ map_instance_windows()               ← Core 预置：组件栈 + 实例窗口
//!   ├─ resolve → begin_start                ← Starting
//!   ├─ runtime slot = 窗口内 runtime block 的实例内 VA
//!   ├─ 写 create args / out_state 进窗口
//!   ├─ isolated::prepare(handle, create, stack, slot, args)
//!   │     └─ Core 验证：入口在 R|X 段、栈被单条 R|W 映射覆盖、gateway 页精确映射
//!   ├─ isolated::enter(...)                 ← 组件在私有 AS 里跑 kcomp_instance_create
//!   └─ Returned(0) → 记录 out_state → 提交 pending → Ready
//!
//! destroy（`exit.rs` 按 execution_domain 分派）
//!   └─ isolated::prepare(handle, image.destroy, stack, slot, (state, 0))
//!      → enter → Returned(0) → retire(handle) → `complete_stop` 提交 Stopped
//! ```
//!
//! # Core 验证 vs 组件提议
//!
//! - **Core 验证**：段规划 / 权限 / 入口 / abi（`isolated_load`）、私有 AS 能力、
//!   生命周期状态机、入口落在可执行映射、栈被可写映射覆盖、gateway 页精确映射
//!   （`isolated::prepare`）、`out_state` 由 Core 从**自己的视图**读回。
//! - **组件提议**：`kcomp_instance_create` 返回的 opaque state（Core 只存）与其
//!   内部行为；`kcomp_instance_destroy` 自行收尾。
//!
//! # 内存路径决定（本增量）：Core 预置窗口，**无 import 面**
//!
//! `docs/architecture/memory-and-heap.md` 的域视图契约
//! （`kcore_memory_acquire` / `release`）要求组件能调到 Core。本增量**不引入**
//! component→Core 的 gate-call trampoline（那需要 `ecall` 分派 + 按域 import
//! 解析 + per-instance VA 预算，属于引入跨域 Gate 的后续增量），因此 Isolated
//! 的 import 包络保持**空集**：任何 UNDEF 符号（含 `kcore_*`）在装载前显式拒绝。
//!
//! 替代机制（本增量选择、明示登记）：Core 预置一块**实例内存窗口**
//! （[`ISOLATED_WINDOW_BASE`]；Core backing、零初始化、只映射在该实例的私有 AS
//! 里），交付 create args / out_state / runtime context。窗口的表示是**实例内
//! VA**——与域视图同形（`view.base/len` 只在那个 AS 里有意义），归属由该实例的
//! 页表承载，Core 不另立账本。窗口**只在创建它的实例 AS 里可达**：同一 VA 在
//! Core AS / 别的实例 AS 里没有任何映射（ArchTest `isolated-lifecycle` 证明）。
//!
//! 窗口布局（**Core 内部**；组件**不需要**知道偏移——它只用 Core 经 `a0` /
//! `a1` / `tp` 交给它的实例内 VA）：
//!
//! ```text
//! +0    KcompCreateArgs（config_abi / config / config_len）
//! +32   out_state 槽（usize；create 返回后由 Core 读回）
//! +64   runtime context block（`tp` 指向这里；Core 从不解释其内容）
//! +128  config 负载拷贝（≤ WINDOW_CONFIG_MAX 字节；config 指针指向这里）
//! +384  本窗口的 **域视图编码**（`kcore_memory_view`：kind = LOCAL_VA、
//!       base = 本实例窗口 VA、len = 窗口长度）——域视图契约的"表示由域承载"
//!       在这里就是这份记录；可调用的 `kcore_memory_acquire` 面仍属后续增量
//! ```
//!
//! # 明确不做（本增量登记）
//!
//! - **组件→Core 的 import 面**：没有 trampoline / `ecall` 分派，`kcore_*` 一律在
//!   装载前拒绝（见上）。跨域 service Gate 与它一起属后续增量。
//! - **destroy 后的物理回收**：AS 退役（复用 = 新建空间）后窗口 backing 保持驻留
//!   （phase 1 逻辑死亡 / 物理驻留，与 image 同）；页表页也没有 teardown 接口。
//! - **同一 artifact 的第二个 Isolated 实例**：image 表按名字唯一且不区分域，
//!   `validate_isolated_load` 仍拒绝复用（按 `(name, domain)` 索引留给后续）。
//! - **ASID / U-mode**：ASID 恒 0 + 全量 `sfence.vma`；没有 U-mode / `ecall`。
//!
//! # 诚实边界
//!
//! - **协作式、非对抗**：S-mode 组件与 Core 同特权级，可以直接改 `satp` / 自己的
//!   映射。本模块不声称对抗隔离（那是 U-mode / SandboxedNative，未实现）。
//! - **ASID 恒 0 + 全量 `sfence.vma`**（arch gateway 的既定边界）。
//! - **CPU isolation ≠ DMA isolation**：Isolated 实例的 AS 只映射自己的镜像 /
//!   机制页 / 栈 / 窗口，**不含**任何 MMIO / Core 段 / 页表 / 别的实例；但
//!   Core 拥有的 DMA backing 是否可被错误复用属于 increment 6 的跨域服务边界，
//!   本增量不声称 DMA 静默。
//! - **失败 / 停止的物理回收**：create 失败时退役 AS 并归还 Core 预置窗口的
//!   backing；destroy 成功后退役 AS（复用 = 新建空间），窗口 backing 保持驻留
//!   （phase 1 逻辑死亡 / 物理驻留，与 image 同）。页表页本身没有 teardown
//!   接口，退役后不再可达即"不 leaked AS"。

use crate::component::ComponentId;
use crate::component::containment::{CallOutcome, KcompCreateArgs};
use crate::component::isolated_load;
use crate::component::load::ComponentLoadError;
use crate::errno::Errno;
use crate::generated::abi::{KCORE_MEMORY_VIEW_LOCAL_VA, MemoryView};
use crate::memory;
use crate::memory::address_space::VirtualRange;

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
/// 地址 / Core 私有 VA。可调用的 `kcore_memory_acquire` 面属后续增量（本增量
/// Core 直接把这一份记录写进窗口交付）。
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

// 布局不变量：镜像窗口（开区间结束）== 栈基址；栈与窗口不重叠。
const _: () = {
    let image_end =
        isolated_load::ISOLATED_IMAGE_WINDOW.base + isolated_load::ISOLATED_IMAGE_WINDOW.size;
    assert!(image_end == ISOLATED_STACK_BASE);
    assert!(ISOLATED_STACK_BASE + ISOLATED_STACK_SIZE <= ISOLATED_WINDOW_BASE);
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
    use crate::component::{failure, image, registry, runtime_slot};
    use crate::memory::address_space::{
        self, AddressSpaceHandle, MapError, Mapping, MappingPermission, PhysicalRange,
    };

    /// 创建并启动一个 Isolated 实例（`load.rs::create_isolated_native` 的实现）。
    ///
    /// 任何一步失败都走"半成品不留"：退役 AS + 归还 Core 预置窗口 backing +
    /// `Failed`（实例一旦声明就一定有终态）。
    pub(crate) fn create(
        name: &[u8],
        blob: &[u8],
        args: &KcompCreateArgs,
    ) -> Result<ComponentId, ComponentLoadError> {
        // (1) 按域放段：段 / 权限 / 入口 / kcomp_abi 全部 Core 验证。
        let placed = isolated_load::place(blob).map_err(map_placement_error)?;
        let mappings = placed.mappings();
        let create_entry = placed.create();

        // (2) image 登记：lease 归 image 表（pinned-until-reboot）；规划副本留在本地。
        let image = image::get_images()
            .lock()
            .register(name, placed.into_loaded_component())
            .map_err(|_| ComponentLoadError::ImageFailed)?;

        // (3) 实例声明（Isolated 域 = 创建入口的分派结果，不是组件自报）。
        let id = registry::get_registry()
            .lock()
            .declare(image, ExecutionDomain::IsolatedNative)
            .map_err(|_| ComponentLoadError::DeclareFailed)?;

        // (4) 私有 AS：还没有 AS 就没有可清理的，直接 Failed。
        let handle = match address_space::create_address_space_for(id) {
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

        // (5) 落镜像段 + Core 预置窗口（栈 / 实例窗口）。
        if let Err(error) = isolated_load::map_mappings(handle, &mappings) {
            return Err(fail_with_as(id, handle, map_placement_error(error)));
        }
        if let Err(error) = map_instance_windows(handle) {
            return Err(fail_with_as(id, handle, error));
        }

        // (6) Declared → Resolved → Starting（create 执行期）。
        let started = {
            let mut reg = registry::get_registry().lock();
            reg.resolve(id).and_then(|()| reg.begin_start(id))
        };
        if started.is_err() {
            return Err(fail_with_as(id, handle, ComponentLoadError::StartFailed));
        }

        // (7) 窗口预置：args / out_state（组件只见实例内 VA）+ runtime slot。
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

        // (8) Core 验证入口 / 栈 / gateway 映射（持锁阶段，返回后不持锁）。
        //     先把 gateway 的组件故障分派接到 Core：**没有显式策略就是 Abandon**
        //     （组件身份本身不是可恢复的证明），create 里的故障因此收敛成
        //     `Outcome::Faulted` → `Failed`，而不是把 Core 打 panic。
        isolated::install();
        let transition = match isolated::prepare(
            handle,
            create_entry,
            stack_range(),
            slot,
            true,
            (
                window.base + WINDOW_ARGS_OFF,
                window.base + WINDOW_OUT_STATE_OFF,
            ),
        ) {
            Ok(transition) => transition,
            Err(error) => return Err(fail_with_as(id, handle, map_prepare_error(error))),
        };

        // (9) 进入：组件在私有 AS 里执行 `kcomp_instance_create(args, out_state)`。
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
            (state as usize, 0),
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
            // gateway 判为不可恢复：按 destroy panic 同档（Failed + 不重试）。
            Outcome::Faulted => CallOutcome::Panicked,
        };
        // 实例已被请求停止：AS 不再可能被进入（复用 = 新建空间），一律退役。
        let _ = address_space::retire(handle);
        outcome
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
        map_window(handle, window_range())
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
        for range in [stack_range(), window_range()] {
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
}

pub(crate) use imp::{create, destroy};

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

    /// 实例窗口 / 栈与镜像窗口不重叠：镜像窗口开区间结束 == 栈基址，栈在窗口之下。
    #[test]
    fn instance_ranges_do_not_overlap_the_image_window() {
        let ranges = [
            isolated_load::ISOLATED_IMAGE_WINDOW,
            stack_range(),
            window_range(),
        ];
        for pair in ranges.windows(2) {
            assert!(
                pair[0].base + pair[0].size <= pair[1].base,
                "instance ranges must not overlap"
            );
        }
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
