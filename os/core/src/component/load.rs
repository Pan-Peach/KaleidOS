//! 组件加载语义入口（ComponentManager 教学版占位）：仓库读取 → loader 放段 →
//! registry 声明 → resolve → start（Ready）→ 调用入口（call_init）。
//!
//! `monitor load <name>` 与组件 ABI `kcore_component_load` 都是这里的**薄 caller**——
//! 加载流程本身属于 Core（monitor 不是 ComponentManager）。完整依赖解析、
//! kpkg manifest requires、失败回滚留给真正的 ComponentManager 里程碑。

use crate::component::loader::{self, LoaderError};
use crate::component::{ComponentId, registry};
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
    /// 状态机拒绝 start（Declared 直接 start 等）。
    StartFailed,
    /// 组件入口返回非零（`kcomp_init` 失败位图）。
    InitFailed(i32),
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
/// 锁纪律：registry 锁只覆盖 declare/resolve/start；`call_init` 在**无锁**
/// 状态下调用（组件 init 可能再 load 别的组件、publish 接口、创建任务，
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
        reg.start(id).map_err(|_| ComponentLoadError::StartFailed)?;
        id
    };

    // 入口调用：期间 CURRENT = 本组件（publish / task_create 的身份来源）。
    let previous = *CURRENT.lock();
    *CURRENT.lock() = Some(id);
    let code = loader::call_init(&comp);
    *CURRENT.lock() = previous;

    if code == 0 {
        Ok(id)
    } else {
        registry::get_registry().lock().mark_failed(id).ok();
        Err(ComponentLoadError::InitFailed(code))
    }
}
