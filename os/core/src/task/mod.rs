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

use crate::component::{ComponentId, ComponentState, containment};

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
/// 1. `requester` 必须存在且处于 `Ready`（运行中）或 `Starting`（`kcomp_instance_create`
///    执行期间，组件可以创建自己的任务）——只有活着的实例能创建任务；
/// 2. `entry` 必须落在该实例 **image 的装载镜像内**（`[base, base + text_size)`）——
///    组件不能把执行权指到任意内核地址，也不能指到别的组件的镜像。
///
/// `arg` 是 opaque 参数：Core 只存/透传给任务入口，**任务归属仍来自 Core 的执行
/// 边界**（`TaskRecord.owner` = requester），不来自 `arg` 内容。
///
/// 通过后由 `TaskTable::create(requester, entry, arg)` 记录 owner + entry + arg，
/// 并分配 id + 内核栈 + 初始上下文（进入 Core trampoline，`Created` 态，
/// 经 `transition(Created→Runnable)` 后进入调度）。
///
/// # Seam
/// caller 身份统一由 `resource::RequestContext::ambient()` 解析（最内层活动执行
/// 边界优先：组件任务 → task owner；`kcomp_instance_create` → 被创建的实例）。
/// 没有真正的 per-execution-domain 凭证（TaskHandle 化未实现）。
pub fn create_task(
    requester: ComponentId,
    entry: usize,
    arg: *mut (),
) -> Result<TaskId, TaskError> {
    // 上下文种类门禁：IRQ 回调（同步、不可 yield 的顶半部）与 service call
    //（provider 跑在 Core 拥有的 service stack 上，没有调度可见的任务）内不得
    // 创建 work（创建任务会分配内核栈 / 新执行流）。门禁沿边界链**祖先遍历**：
    // 嵌套的 init / exit / task 边界不能把 IRQ 或 service 祖先藏起来。Core 机制
    // 层拒绝，返回 `-EINVAL`（复用 `InvalidTransition`，不改内部错误枚举与唯一
    // errno 映射表），绝不 panic。
    if containment::scheduling_forbidden() {
        return Err(TaskError::InvalidTransition);
    }
    // 单锁：provider 自己的 loaded image 就在记录里，无需二次查表。
    let inside_image = {
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
        let base = record.loaded.base;
        let end = base + record.loaded.text_size;
        entry >= base && entry < end
    };
    if !inside_image {
        return Err(TaskError::EntryOutOfImage);
    }

    get_task_table().lock().create(requester, entry, arg)
}

/// Core 语义入口：启动任务（Created → Runnable）。
///
/// 任务 ID 只是可猜测的 identity；Core 必须在状态转换前验证 requester
/// 是否等于任务记录中的 owner。
///
/// 上下文种类门禁：IRQ 回调作用域或 service-call 边界内拒绝启动任务
/// （`-EINVAL`），理由同 [`create_task`]。
pub fn start_task(requester: ComponentId, task: TaskId) -> Result<(), TaskError> {
    if containment::scheduling_forbidden() {
        return Err(TaskError::InvalidTransition);
    }
    get_task_table().lock().start(requester, task)
}

/// Core 拥有的任务入口 trampoline：按 `typedef void (*KcompTaskEntry)(void *)`
/// 契约调用组件任务函数，`arg` **原样透传**（Core 不解引用）。
///
/// - 任务归属来自 Core 的执行边界（`TaskRecord.owner`），与 `arg` 无关；
/// - 组件任务 panic 时由 containment 的 task 边界接管（scheduler 进入前已装
///   escape guard），控制权切回 Core abort 上下文；
/// - 入口返回违反"必须经 Core 退出"的契约：Core 兜底按 exit 处理，**绝不恢复
///   该任务**（返回后自旋，等调度器切走）。
extern "C" fn task_entry_trampoline() -> ! {
    let Some(id) = crate::sched::current_task() else {
        halt()
    };
    let (entry, arg) = {
        let table = get_task_table().lock();
        match table.get(id) {
            Some(record) => (record.entry(), record.arg()),
            // 不变式：被调度运行的任务必然在表里。
            None => halt(),
        }
    };
    // SAFETY: `entry` 由 `kcore_task_create` 提供并已通过"落在 owner 镜像内"
    // 验证；签名契约 = SDK 侧 `KcompTaskEntry`（`extern "C" fn(*mut ())`）。
    let task_entry: extern "C" fn(*mut ()) = unsafe { core::mem::transmute(entry) };
    task_entry(arg);
    // 契约要求任务必须经 `kcore_task_exit` 退出；返回视为 Core 兜底退出。
    let _ = crate::sched::exit_current();
    halt()
}

fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::endpoint::ExecutionDomain;
    use crate::component::registry;
    use crate::memory::test_support;

    /// 全局表（registry / task）是进程级 `Once`，`init()` 幂等。会分配装载
    /// 镜像 lease / kstack 的用例必须持有 memory GUARD，串行化全局堆。
    fn setup() -> test_support::Guard<'static> {
        registry::init();
        crate::task::init();
        let guard = test_support::GUARD.lock();
        test_support::ensure_init();
        guard
    }

    /// 一份带真实常驻 backing 的伪造 loaded image（`base..base+text_size` 是合法
    /// entry 区间）；调用方须已持有 memory GUARD。
    fn test_loaded() -> crate::component::loader::LoadedComponent {
        crate::component::registry::test_support::test_loaded_with_region(0, None)
    }

    /// 声明一个带真实 backing 的 `Starting` 组件，返回 `(id, base, size)`。
    fn starting_component(name: &[u8]) -> (ComponentId, usize, usize) {
        let mut reg = registry::get_registry().lock();
        let id = reg
            .declare(name, test_loaded(), ExecutionDomain::KernelNative)
            .expect("declare starting component");
        reg.resolve(id).expect("resolve");
        reg.begin_start(id).expect("begin_start");
        drop(reg);
        let reg = registry::get_registry().lock();
        let record = reg.get(id).expect("record");
        (id, record.loaded.base, record.loaded.text_size)
    }

    #[test]
    fn create_task_rejects_undeclared_requester() {
        // Given: 一个从未声明的身份（ID 可被猜测，但存在性由 Core 验证）。
        let _boundary = containment::test_boundary_lock();
        let _g = setup();
        let ghost = ComponentId::from_raw(0xFFFF_FF00);
        let before = get_task_table().lock().len();

        // When: 它请求创建任务。
        let result = create_task(ghost, 0x1000, core::ptr::null_mut());

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
        // Given: 三个存在但非 Starting/Ready 的组件。
        let _boundary = containment::test_boundary_lock();
        let _g = setup();
        let before = get_task_table().lock().len();

        // Declared：只声明。
        let declared = registry::get_registry()
            .lock()
            .declare(
                b"task_perm_states",
                test_loaded(),
                ExecutionDomain::KernelNative,
            )
            .unwrap();
        // Failed：走完 init 路径后逻辑死亡。
        let failed = {
            let mut reg = registry::get_registry().lock();
            let id = reg
                .declare(
                    b"task_perm_states",
                    test_loaded(),
                    ExecutionDomain::KernelNative,
                )
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
                .declare(
                    b"task_perm_states",
                    test_loaded(),
                    ExecutionDomain::KernelNative,
                )
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
            create_task(declared, 0x1000, core::ptr::null_mut()),
            Err(TaskError::RequesterNotReady)
        );
        assert_eq!(
            create_task(failed, 0x1000, core::ptr::null_mut()),
            Err(TaskError::RequesterNotReady)
        );
        assert_eq!(
            create_task(stopped, 0x1000, core::ptr::null_mut()),
            Err(TaskError::RequesterNotReady)
        );
        assert_eq!(get_task_table().lock().len(), before);
    }

    #[test]
    fn create_task_rejects_entry_outside_loaded_image() {
        // Given: 一个带装载镜像 lease（[base, base+size)）的 Starting 组件。
        let _boundary = containment::test_boundary_lock();
        let _g = setup();
        let (id, base, size) = starting_component(b"task_perm_entry");
        let before = get_task_table().lock().len();
        let below = base.checked_sub(1).expect("host image base is never zero");
        let end = base + size;

        // When/Then: 镜像下方一字节、末地址本身（右开区间）与更远处一律拒绝。
        assert_eq!(
            create_task(id, below, core::ptr::null_mut()),
            Err(TaskError::EntryOutOfImage)
        );
        assert_eq!(
            create_task(id, end, core::ptr::null_mut()),
            Err(TaskError::EntryOutOfImage),
            "base+size 是排他上界"
        );
        assert_eq!(
            create_task(id, end + 1, core::ptr::null_mut()),
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
    fn create_task_accepts_entry_inside_loaded_image() {
        // Given: 一个带装载镜像的 Starting 组件。
        let _boundary = containment::test_boundary_lock();
        let _g = setup();
        let (id, base, size) = starting_component(b"task_perm_ok");
        let entry = base + 0x80;
        assert!(entry < base + size, "entry 必须落在镜像内");
        let mut arg = 0u32;
        let arg_ptr = core::ptr::addr_of_mut!(arg).cast::<()>();

        // When: 请求创建任务（带 opaque arg）。
        let task = create_task(id, entry, arg_ptr).expect("镜像内的 entry 应被接受");

        // Then: Core 记录了一个 Created 任务，owner 就是 requester，entry/arg 原样。
        {
            let table = get_task_table().lock();
            assert!(table.contains(task), "任务已登记");
            let record = table.get(task).expect("记录存在");
            assert_eq!(record.owner(), id, "owner 即 requester");
            assert_eq!(record.state(), TaskState::Created, "新任务以 Created 起步");
            assert_eq!(record.entry(), entry, "entry 原样保存");
            assert_eq!(record.arg(), arg_ptr, "arg 原样保存（归属与 arg 无关）");
        }

        // 清理：移除任务会归还 kstack 区域。
        get_task_table().lock().remove(task).expect("cleanup");
    }

    /// 契约核心：同一个 artifact 的两个组件各自拥有独立任务与独立 loaded image；
    /// owner 是组件 id。
    #[test]
    fn same_artifact_components_own_tasks_independently() {
        // Given：两个独立 declare 的同名组件（各自一份 backing）。
        let _boundary = containment::test_boundary_lock();
        let _g = setup();
        let (first, first_base, first_size) = starting_component(b"task_share_image");
        let (second, second_base, _second_size) = starting_component(b"task_share_image");

        // When：两个组件各创建一个任务，各自携带不同 arg。
        let mut arg_a = 1u32;
        let mut arg_b = 2u32;
        let a = create_task(
            first,
            first_base + 0x40,
            core::ptr::addr_of_mut!(arg_a).cast::<()>(),
        )
        .unwrap();
        let b = create_task(
            second,
            second_base + 0x40,
            core::ptr::addr_of_mut!(arg_b).cast::<()>(),
        )
        .unwrap();

        // Then：owner 各归其组件；两个组件各自独立持有自己的任务。
        let table = get_task_table().lock();
        assert_eq!(table.get(a).unwrap().owner(), first);
        assert_eq!(table.get(b).unwrap().owner(), second);
        assert!(table.has_live_tasks(first), "first 拥有自己的任务");
        assert!(table.has_live_tasks(second), "second 拥有自己的任务");
        drop(table);
        assert_ne!(first_base, second_base, "两个组件各自独立 backing");
        let _ = first_size;

        // 清理。
        get_task_table().lock().remove(a).unwrap();
        get_task_table().lock().remove(b).unwrap();
    }

    #[test]
    fn start_task_rejects_wrong_owner() {
        // Given: 一个属于某组件的任务。
        let _boundary = containment::test_boundary_lock();
        let _g = setup();
        let (owner, base, _size) = starting_component(b"task_start_wrong_owner");
        let task = create_task(owner, base, core::ptr::null_mut()).unwrap();
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
        let _boundary = containment::test_boundary_lock();
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
        let _boundary = containment::test_boundary_lock();
        let _g = setup();
        let (owner, base, _size) = starting_component(b"task_start_ok");
        let task = create_task(owner, base, core::ptr::null_mut()).unwrap();
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

    /// 上下文种类门禁：IRQ 回调作用域内不得创建/启动任务 —— 两个语义入口都
    /// 返回 `-EINVAL`（负 errno），且不在真相上产生任何任务（拒绝而非 panic）。
    ///
    /// 不取 `setup()` 的 memory GUARD：本用例不分配，而调度测试的锁序是
    /// boundary → memory，这里若 memory → boundary 会与它们死锁。
    #[test]
    fn task_create_and_start_are_rejected_in_irq_context() {
        crate::task::init();
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        let requester = ComponentId::from_raw(0xfeed);

        containment::with_irq_scope(ComponentId::from_raw(9), || {
            assert_eq!(
                create_task(requester, 0x1000, core::ptr::null_mut()),
                Err(TaskError::InvalidTransition)
            );
            assert_eq!(
                start_task(requester, TaskId::from_raw(1)),
                Err(TaskError::InvalidTransition)
            );
            assert_eq!(
                crate::errno::Errno::from(TaskError::InvalidTransition).code(),
                -22,
                "ABI 上是负 errno（EINVAL），不是 panic"
            );
        });

        assert!(
            !get_task_table().lock().has_live_tasks(requester),
            "IRQ 上下文的拒绝不得创建任务"
        );
        containment::enter_anchor();
    }

    /// 上下文种类门禁（祖先感知）：service-call 边界内（含嵌套 init 之下）
    /// 创建 / 启动任务同样被拒（`-EINVAL`），绝不产生任务。
    #[test]
    fn task_create_and_start_are_rejected_under_a_service_call_ancestor() {
        crate::task::init();
        let _boundary = containment::test_boundary_lock();
        containment::enter_anchor();
        let requester = ComponentId::from_raw(0xfeed);

        containment::with_test_service_boundary(
            ComponentId::from_raw(0xB),
            crate::component::endpoint::EndpointId::from_raw(1),
            None,
            || {
                containment::with_test_init_boundary(Some(ComponentId::from_raw(0xC)), || {
                    assert_eq!(
                        create_task(requester, 0x1000, core::ptr::null_mut()),
                        Err(TaskError::InvalidTransition)
                    );
                    assert_eq!(
                        start_task(requester, TaskId::from_raw(1)),
                        Err(TaskError::InvalidTransition)
                    );
                });
            },
        );

        assert!(
            !get_task_table().lock().has_live_tasks(requester),
            "service-call 上下文的拒绝不得创建任务"
        );
        containment::enter_anchor();
    }
}
