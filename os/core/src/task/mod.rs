//! Task 真相：身份（TaskId）、owner、状态、运行 CPU、内核栈、上下文。
//! 调度策略数据（runqueue、vruntime 等）不在此模块 —— 属于 Scheduler Component。
//! 调度 commit 路径（propose → validate → commit → switch）在 `crate::sched`。

pub mod error;
pub mod id;
pub mod kstack;
pub mod record;
pub mod state;
pub mod table;

pub use error::TaskError;
pub use id::TaskId;
pub use kstack::Kernelstack;
pub use record::TaskRecord;
pub use state::TaskState;
pub use table::TaskTable;

use crate::component::{ComponentId, ComponentState};

pub static TASK_TABLE: spin::Once<spin::Mutex<TaskTable>> = spin::Once::new();

pub fn init() {
    TASK_TABLE.call_once(|| spin::Mutex::new(TaskTable::new()));
}

pub fn get_task_table() -> &'static spin::Mutex<TaskTable> {
    TASK_TABLE.get().expect("task table not initialized")
}

/// Core 语义入口：创建任务（组件只能经 export ABI `kcore_task_create` 到达）。
///
/// 验证（Core validates，组件只有提议权）：
/// 1. `requester` 必须存在且处于 `Ready`（运行中）或 `Starting`（`kcomp_init`
///    执行期间，组件可以创建自己的任务）——只有活着的组件能创建任务；
/// 2. `entry` 必须落在该组件的**装载镜像内**（`[base, base+size)`）——
///    组件不能把执行权指到任意内核地址，也不能指到别的组件的镜像。
///
/// 通过后由 `TaskTable::create(requester, ...)` 记录 owner，并分配 id + 内核栈
/// + 初始上下文（`Created` 态，经 `transition(Created→Runnable)` 后进入调度）。
///
/// # Seam
/// caller 身份统一由 `handle::RequestContext::ambient()` 解析（最内层活动执行
/// 边界优先：组件任务 → task owner；`kcomp_init` → 被初始化组件）。真正的
/// per-execution-domain 凭证（TaskHandle 化）留给未来 ExecutionDomain 里程碑。
pub fn create_task(requester: ComponentId, entry: usize) -> Result<TaskId, TaskError> {
    let registry = crate::component::registry::get_registry().lock();
    let record = registry
        .get(requester)
        .ok_or(TaskError::RequesterNotFound)?;
    if !matches!(
        record.state,
        ComponentState::Starting | ComponentState::Ready
    ) {
        return Err(TaskError::RequesterNotReady);
    }
    let Some(lease) = &record.memory else {
        // 无装载镜像（不应发生：Ready 组件必然已经 loader 放段）。
        return Err(TaskError::EntryOutOfImage);
    };
    let image = lease.region();
    if entry < image.base || entry >= image.base + image.size {
        return Err(TaskError::EntryOutOfImage);
    }
    drop(registry);

    get_task_table().lock().create(requester, entry)
}

/// Core 语义入口：启动任务（Created → Runnable）。
///
/// 任务 ID 只是可猜测的 identity；Core 必须在状态转换前验证 requester
/// 是否等于任务记录中的 owner。
pub fn start_task(requester: ComponentId, task: TaskId) -> Result<(), TaskError> {
    get_task_table().lock().start(requester, task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::registry;
    use crate::memory;
    use crate::memory::test_support;

    /// 全局表（registry / task）是进程级 `Once`，`init()` 幂等。会分配装载镜像
    /// lease / kstack 的用例必须持有 memory GUARD，串行化全局堆。组件名**每例唯一**，
    /// 避免与其它用例（含 registry 自身测试）撞名导致 `declare` 冲突。
    fn setup() -> test_support::Guard<'static> {
        registry::init();
        crate::task::init();
        let guard = test_support::GUARD.lock();
        test_support::ensure_init();
        guard
    }

    /// 声明一个 `Starting` 组件并挂上装载镜像 lease，返回 `(id, base, size)`。
    ///
    /// 调用方须已持有 memory GUARD（分配 lease）。
    fn starting_component(name: &[u8]) -> (ComponentId, usize, usize) {
        let lease = memory::alloc_region(memory::ALLOC_GRANULE).expect("image lease");
        let image = lease.region();
        let (base, size) = (image.base, image.size);
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(name, base, base, Some(lease))
            .expect("declare starting component");
        reg.resolve(id).expect("resolve");
        reg.begin_start(id).expect("begin_start");
        (id, base, size)
    }

    #[test]
    fn create_task_rejects_undeclared_requester() {
        // Given: 一个从未声明的身份（ID 可被猜测，但存在性由 Core 验证）。
        let _g = setup();
        let ghost = ComponentId::from_raw(0xFFFF_FF00);
        let before = get_task_table().lock().len();

        // When: 它请求创建任务。
        let result = create_task(ghost, 0x1000);

        // Then: 存在性验证拒绝，且真相上不产生任何任务。
        assert_eq!(result, Err(TaskError::RequesterNotFound));
        assert_eq!(
            get_task_table().lock().len(),
            before,
            "被拒绝的 requester 不得创建任务"
        );
    }

    #[test]
    fn create_task_rejects_requester_that_is_not_live() {
        // Given: 三个存在但非 Starting/Ready 的实例。
        let _g = setup();
        let before = get_task_table().lock().len();

        // Declared：只声明。
        let declared = {
            let mut reg = registry::get_registry().lock();
            reg.declare(b"task_perm_declared", 0x1000, 0x2000, None)
                .unwrap()
        };
        // Failed：走完 init 路径后逻辑死亡。
        let failed = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(b"task_perm_failed", 0x1000, 0x2000, None)
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.mark_failed(id).unwrap();
            id
        };
        // Stopped：完整初始化后优雅停止。
        let stopped = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(b"task_perm_stopped", 0x1000, 0x2000, None)
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            reg.finish_start(id).unwrap();
            reg.begin_stop(id).unwrap();
            reg.finish_stop(id).unwrap();
            id
        };

        // When/Then: 只有活着的组件能创建任务；三种非活状态一律拒绝。
        assert_eq!(
            create_task(declared, 0x1000),
            Err(TaskError::RequesterNotReady)
        );
        assert_eq!(
            create_task(failed, 0x1000),
            Err(TaskError::RequesterNotReady)
        );
        assert_eq!(
            create_task(stopped, 0x1000),
            Err(TaskError::RequesterNotReady)
        );
        assert_eq!(get_task_table().lock().len(), before);
    }

    #[test]
    fn create_task_rejects_entry_outside_loaded_image() {
        // Given: 一个带装载镜像 lease（[base, base+size)）的 Starting 组件。
        let _g = setup();
        let (id, base, size) = starting_component(b"task_perm_entry");
        let before = get_task_table().lock().len();
        let below = base.checked_sub(1).expect("host image base is never zero");
        let end = base + size;

        // When/Then: 镜像下方一字节、末地址本身（右开区间）与更远处一律拒绝。
        assert_eq!(create_task(id, below), Err(TaskError::EntryOutOfImage));
        assert_eq!(
            create_task(id, end),
            Err(TaskError::EntryOutOfImage),
            "base+size 是排他上界"
        );
        assert_eq!(
            create_task(id, end + 1),
            Err(TaskError::EntryOutOfImage),
            "镜像之外"
        );
        assert_eq!(
            get_task_table().lock().len(),
            before,
            "被拒绝的 entry 不创建任务"
        );
    }

    #[test]
    fn create_task_rejects_starting_component_without_image() {
        // Given: 一个 Starting 但未挂载镜像（memory == None）的组件。
        let _g = setup();
        let id = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(b"task_perm_no_image", 0x1000, 0x2000, None)
                .unwrap();
            reg.resolve(id).unwrap();
            reg.begin_start(id).unwrap();
            id
        };
        let before = get_task_table().lock().len();

        // When/Then: 没有镜像就没有合法 entry —— 拒绝而非猜测。
        assert_eq!(create_task(id, 0x1000), Err(TaskError::EntryOutOfImage));
        assert_eq!(get_task_table().lock().len(), before);
    }

    #[test]
    fn create_task_accepts_entry_inside_loaded_image() {
        // Given: 一个带装载镜像的 Starting 组件。
        let _g = setup();
        let (id, base, size) = starting_component(b"task_perm_ok");
        let entry = base + 0x80;
        assert!(entry < base + size, "entry 必须落在镜像内");

        // When: 请求创建任务。
        let task = create_task(id, entry).expect("镜像内的 entry 应被接受");

        // Then: Core 记录了一个 Created 任务，owner 就是 requester。
        {
            let table = get_task_table().lock();
            assert!(table.contains(task), "任务已登记");
            let record = table.get(task).expect("记录存在");
            assert_eq!(record.owner(), id, "owner 即 requester");
            assert_eq!(record.state(), TaskState::Created, "新任务以 Created 起步");
        }

        // 清理：移除任务会归还 kstack 区域。
        get_task_table().lock().remove(task).expect("cleanup");
    }

    #[test]
    fn start_task_rejects_wrong_owner() {
        // Given: 一个属于某组件的任务。
        let _g = setup();
        let (owner, base, _size) = starting_component(b"task_start_wrong_owner");
        let task = create_task(owner, base).unwrap();
        let intruder = ComponentId::from_raw(0xDEAD_BEEF);

        // When: 另一个身份尝试启动它。
        let result = start_task(intruder, task);

        // Then: 所有权被强制，任务保持 Created（被拒绝的启动不改真相）。
        assert_eq!(result, Err(TaskError::WrongOwner));
        assert_eq!(
            get_task_table().lock().get(task).unwrap().state(),
            TaskState::Created,
            "被拒绝的 start 不得改变状态"
        );

        get_task_table().lock().remove(task).unwrap();
    }

    #[test]
    fn start_task_unknown_id_is_not_found() {
        // Given: 表内没有该 id（ID 可被猜测；存在性先于所有权验证）。
        let _g = setup();
        let ghost = TaskId::from_raw(0xFFFF_FFFF);

        // When/Then: 查无此任务 → NotFound。
        assert_eq!(
            start_task(ComponentId::from_raw(0xDEAD_BEEF), ghost),
            Err(TaskError::NotFound)
        );
    }

    #[test]
    fn start_task_by_owner_moves_created_to_runnable() {
        // Given: 一个由活组件拥有的 Created 任务。
        let _g = setup();
        let (owner, base, _size) = starting_component(b"task_start_ok");
        let task = create_task(owner, base).unwrap();
        assert_eq!(
            get_task_table().lock().get(task).unwrap().state(),
            TaskState::Created
        );

        // When: owner 启动它。
        let result = start_task(owner, task);

        // Then: Created → Runnable（唯一合法的首次转换）。
        assert_eq!(result, Ok(()));
        assert_eq!(
            get_task_table().lock().get(task).unwrap().state(),
            TaskState::Runnable
        );

        get_task_table().lock().remove(task).unwrap();
    }
}
