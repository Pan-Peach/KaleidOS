//! 组件加载语义入口（ComponentManager 教学版占位）：仓库读取 → loader 放段 →
//! registry 声明 → resolve → begin_start（Starting）→ 调用入口（call_init）→
//! 成功则原子提交 pending interfaces 并 finish_start（Ready）。
//!
//! `monitor load <name>` 与组件 ABI `kcore_component_load` 都是这里的**薄 caller**——
//! 加载流程本身属于 Core（monitor 不是 ComponentManager）。完整依赖解析、
//! kpkg manifest requires、失败回滚留给真正的 ComponentManager 里程碑。

use crate::component::interface::{self, InterfaceError};
use crate::component::loader::{self, LoaderError};
use crate::component::{ComponentId, containment, failure, registry};
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
    /// ELF 解析 / 放段 / 重定位失败。
    Loader(LoaderError),
    /// registry 声明失败（重名 / 名字过长 / id 耗尽）。
    DeclareFailed,
    /// resolve 失败（require 未满足；v1 无 requires，不应发生）。
    ResolveFailed,
    /// 状态机拒绝 begin_start/finish_start。
    StartFailed,
    /// 组件入口返回非零（`kcomp_init` 失败位图）。
    InitFailed(i32),
    /// 组件入口 panic 已切回 Core；组件状态由 caller 提交为 Failed。
    InitPanicked,
    /// `kcomp_init` 返回 0，但 pending interfaces 提交冲突（ABI mismatch /
    /// kind mismatch）——组件被提交为 Failed，旧 binding 不受影响。
    InterfaceCommitFailed(InterfaceError),
    /// 组件拥有的任务 panic，已由 task-abort 上下文提交为 `Exited`；
    /// 组件的 authority 由 abort 路径撤销（仅作 reason 语义）。
    TaskPanicked(TaskId),
}

/// 当前正在初始化的组件（call_init 期间由 Core 记录）。
///
/// `kcore_interface_publish` 的 provider 以及锚点上 `call_init` 阶段的 task
/// requester 从这里解析——组件不需要知道自己/别人的 ComponentId，Core 不信任
/// 组件自报的身份。普通任务的 requester 从 `TaskRecord.owner` 解析。嵌套加载
///（组件 init 里再 load 别的组件）时保存/恢复。
static CURRENT: Mutex<Option<ComponentId>> = Mutex::new(None);

/// 取当前正在初始化的组件；不在 call_init 内返回 None。
pub fn current_component() -> Option<ComponentId> {
    *CURRENT.lock()
}

/// 加载并启动组件：完整生命周期链，返回组件 id。
///
/// 生命周期：`Declared → resolve → Resolved → begin_start → Starting →
/// call_init → { failure → Failed | success → commit pending interfaces → Ready }`。
///
/// 锁纪律：registry 锁只覆盖 declare/resolve/begin_start/finish_start；`call_init`
/// 在**无锁**状态下调用（组件 init 可能再 load 别的组件、publish 接口、创建任务，
/// 都各自拿锁——不能有任何锁跨 call_init 持有）。
pub fn load_and_start(name: &[u8]) -> Result<ComponentId, ComponentLoadError> {
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

    let mut comp = loader::load_component(&blob).map_err(ComponentLoadError::Loader)?;

    let id = {
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(name, comp.entry, comp.base, comp.take_memory())
            .map_err(|_| ComponentLoadError::DeclareFailed)?;
        reg.resolve(id)
            .map_err(|_| ComponentLoadError::ResolveFailed)?;
        // Resolved → Starting：`kcomp_init` 执行期间 publish 只记录 pending。
        reg.begin_start(id)
            .map_err(|_| ComponentLoadError::StartFailed)?;
        id
    };

    // 入口调用：期间 CURRENT = 本组件（publish / task_create 的身份来源）。
    let previous = *CURRENT.lock();
    *CURRENT.lock() = Some(id);
    let outcome = containment::call_component_init(comp.entry);
    *CURRENT.lock() = previous;

    match outcome {
        containment::CallOutcome::Returned(0) => {
            // init 成功：原子提交 pending interfaces，成功才进入 Ready。
            let committed = {
                let reg = registry::get_registry().lock();
                let mut ifs = interface::get_interfaces().lock();
                ifs.commit_pending(&reg, id)
            };
            match committed {
                Ok(()) => {
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
                Err(interface_error) => {
                    let error = ComponentLoadError::InterfaceCommitFailed(interface_error);
                    failure::fail_component(id, error);
                    Err(error)
                }
            }
        }
        containment::CallOutcome::Returned(code) => {
            let error = ComponentLoadError::InitFailed(code);
            failure::fail_component(id, error);
            Err(error)
        }
        containment::CallOutcome::Panicked => {
            let error = ComponentLoadError::InitPanicked;
            failure::fail_component(id, error);
            Err(error)
        }
    }
}
