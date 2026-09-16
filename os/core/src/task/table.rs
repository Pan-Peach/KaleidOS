//! 任务真相存储：Core 唯一的任务清单（id → TaskRecord）。

use crate::component::ComponentId;
use crate::memory;
use crate::task::error::TaskError;
use crate::task::id::TaskId;
use crate::task::kstack::Kernelstack;
use crate::task::record::TaskRecord;
use crate::task::state::TaskState;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use arch::{CpuArch, CpuImpl};
use core::sync::atomic::{AtomicU32, Ordering};

pub struct TaskTable {
    tasks: BTreeMap<TaskId, TaskRecord>,
    next_id: AtomicU32,
}

impl Default for TaskTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskTable {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(0),
            tasks: BTreeMap::new(),
        }
    }

    /// 唯二创建入口（public）：记录 owner，分配 id + 登记 record。
    pub fn create(&mut self, owner: ComponentId, entry: usize) -> Result<TaskId, TaskError> {
        let id = self.alloc();
        let memory =
            memory::alloc_region(memory::ALLOC_GRANULE).map_err(|_| TaskError::NoMemory)?;
        let region = memory.region();
        let kstack = Kernelstack::new(region.base, memory::ALLOC_GRANULE);
        let context = CpuImpl::new_context(entry, kstack.base + kstack.size);
        let record = TaskRecord::new(owner, Box::new(context), kstack, memory);
        self.insert(id, record)?;
        Ok(id)
    }

    fn alloc(&self) -> TaskId {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        TaskId::from_raw(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&TaskId, &TaskRecord)> {
        self.tasks.iter()
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn contains(&self, id: TaskId) -> bool {
        self.tasks.contains_key(&id)
    }

    /// `owner` 是否仍拥有**未退出**的任务（只读扫表；不引入第二账本）。
    ///
    /// `Exited` 是唯一终态：`yield` 只提交 `Runnable`、不会"自然退出"，
    /// 所以除 `Exited` 外的一切状态（`Created`/`Runnable`/`Running`/`Blocked`）
    /// 都算未完成。停止编排（`component/exit.rs::stop_component`）用它做拒绝门；
    /// 它不提供 join / 等待，也不改变任何任务状态。
    pub fn has_live_tasks(&self, owner: ComponentId) -> bool {
        self.tasks
            .iter()
            .any(|(_, record)| record.owner() == owner && record.state() != TaskState::Exited)
    }

    fn insert(&mut self, id: TaskId, record: TaskRecord) -> Result<(), TaskError> {
        match self.tasks.entry(id) {
            Entry::Vacant(v) => {
                v.insert(record);
                Ok(())
            }
            Entry::Occupied(_) => Err(TaskError::AlreadyExists),
        }
    }

    pub fn get(&self, id: TaskId) -> Option<&TaskRecord> {
        self.tasks.get(&id)
    }

    pub fn get_mut(&mut self, id: TaskId) -> Option<&mut TaskRecord> {
        self.tasks.get_mut(&id)
    }

    /// Core 语义入口：只有任务 owner 才能启动该任务。
    pub fn start(&mut self, requester: ComponentId, id: TaskId) -> Result<(), TaskError> {
        let record = self.get(id).ok_or(TaskError::NotFound)?;
        if record.owner() != requester {
            return Err(TaskError::WrongOwner);
        }
        self.transition(id, TaskState::Runnable)
    }

    /// 状态推进的唯一入口（Core 校验合法转换后才落笔；调度器 commit 路径调用）。
    ///
    /// 合法转换（v1 状态机）：
    /// - `Created → Runnable`（start：任务首次交给调度器）
    /// - `Runnable → Running(cpu)`（dispatch：被调度器选中）
    /// - `Running → Runnable`（yield / 时间片到）
    /// - `Running → Exited`（exit：任务自行退出）
    ///
    /// 其余一律 `InvalidTransition`（Exited 终态、Created 直接 Running 等）。
    /// Running(cpu) 互斥、跨 CPU 检查留给 SMP 里程碑。
    pub fn transition(&mut self, id: TaskId, to: TaskState) -> Result<(), TaskError> {
        let record = self.get_mut(id).ok_or(TaskError::NotFound)?;
        let legal = matches!(
            (&record.state(), &to),
            (TaskState::Created, TaskState::Runnable)
                | (TaskState::Runnable, TaskState::Running(_))
                | (TaskState::Running(_), TaskState::Runnable)
                | (TaskState::Running(_), TaskState::Exited)
        );
        if !legal {
            return Err(TaskError::InvalidTransition);
        }
        record.set_state(to);
        Ok(())
    }

    pub fn remove(&mut self, id: TaskId) -> Result<TaskRecord, TaskError> {
        match self.tasks.remove(&id) {
            Some(record) => Ok(record),
            None => Err(TaskError::NotFound),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::CpuId;
    use crate::memory::test_support;
    use crate::task::state::TaskState;
    use alloc::vec::Vec;

    const ENTRY: usize = 0x8000_0000;
    const OWNER: ComponentId = ComponentId::from_raw(1);
    const OTHER_OWNER: ComponentId = ComponentId::from_raw(2);

    fn setup() -> test_support::Guard<'static> {
        test_support::ensure_init();
        test_support::GUARD.lock()
    }

    #[test]
    fn new_and_default_are_empty() {
        let mut t = TaskTable::new();
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert_eq!(t.iter().next(), None);
        assert_eq!(t.remove(TaskId::from_raw(0)), Err(TaskError::NotFound));

        let d = TaskTable::default();
        assert!(d.is_empty());
    }

    #[test]
    fn create_get_roundtrip() {
        let _g = setup();

        let mut t = TaskTable::new();
        let id = t.create(OWNER, ENTRY).expect("create");
        assert_eq!(id.raw(), 0, "first id is 0");

        assert_eq!(t.len(), 1);
        assert!(t.contains(id));
        let rec = t.get(id).expect("get after create");
        assert_eq!(rec.owner(), OWNER);
        assert_eq!(rec.state(), TaskState::Created);
        assert!(
            rec.kstack.base.is_multiple_of(memory::ALLOC_GRANULE),
            "kstack base page-aligned"
        );
        assert_eq!(rec.kstack.size, memory::ALLOC_GRANULE);
    }

    #[test]
    fn state_is_core_controlled_not_callers_choice() {
        let _g = setup();

        let mut t = TaskTable::new();
        let id = t.create(OWNER, ENTRY).unwrap();
        // 组件/外部 crate 拿不到 &mut state：只能走 Core 的 transition 写入点。
        assert!(matches!(t.get(id).unwrap().state(), TaskState::Created));
        t.transition(id, TaskState::Runnable).unwrap();
        assert!(matches!(t.get(id).unwrap().state(), TaskState::Runnable));
        t.transition(id, TaskState::Running(CpuId(0))).unwrap();
        assert!(matches!(t.get(id).unwrap().state(), TaskState::Running(_)));
    }

    #[test]
    fn transition_rejects_illegal_state_moves() {
        let _g = setup();

        let mut t = TaskTable::new();
        let id = t.create(OWNER, ENTRY).unwrap();

        // Created 直接 Running / Exited：非法（必须经 Runnable / 先跑起来）。
        assert_eq!(
            t.transition(id, TaskState::Running(CpuId(0))),
            Err(TaskError::InvalidTransition)
        );
        assert_eq!(
            t.transition(id, TaskState::Exited),
            Err(TaskError::InvalidTransition)
        );
        // 未存在的 id：NotFound（存在性验证）。
        assert_eq!(
            t.transition(TaskId::from_raw(99), TaskState::Runnable),
            Err(TaskError::NotFound)
        );

        // 合法全链：Created → Runnable → Running → Runnable → Running → Exited。
        t.transition(id, TaskState::Runnable).unwrap();
        t.transition(id, TaskState::Running(CpuId(0))).unwrap();
        t.transition(id, TaskState::Runnable).unwrap();
        t.transition(id, TaskState::Running(CpuId(0))).unwrap();
        t.transition(id, TaskState::Exited).unwrap();
        // Exited 终态：任何推进都非法。
        assert_eq!(
            t.transition(id, TaskState::Runnable),
            Err(TaskError::InvalidTransition)
        );
    }

    #[test]
    fn sequential_ids_are_unique_and_iterated_in_order() {
        let _g = setup();

        let mut t = TaskTable::new();
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(t.create(OWNER, ENTRY).expect("create").raw());
        }
        assert_eq!(ids, [0, 1, 2], "sequential unique ids");

        let iter: Vec<u32> = t.iter().map(|(id, _)| id.raw()).collect();
        assert_eq!(iter, [0, 1, 2], "BTreeMap iterates in id order");
    }

    #[test]
    fn remove_returns_record_and_empties() {
        let _g = setup();

        let mut t = TaskTable::new();
        let id = t.create(OWNER, ENTRY).unwrap();
        let rec = t.remove(id).expect("remove");
        assert_eq!(rec.kstack.size, memory::ALLOC_GRANULE);
        assert!(t.is_empty());
        assert_eq!(
            t.remove(id),
            Err(TaskError::NotFound),
            "second remove fails"
        );
    }

    #[test]
    fn create_reports_no_memory_without_insertion() {
        let _g = setup();

        let mut t = TaskTable::new();
        let mut held = Vec::new();
        while let Ok(lease) = memory::alloc_region(memory::ALLOC_GRANULE) {
            held.push(lease);
        }
        assert!(matches!(t.create(OWNER, ENTRY), Err(TaskError::NoMemory)));
        assert!(t.is_empty(), "failed create must not register");

        drop(held);
    }

    #[test]
    fn insert_rejects_duplicate_without_overwrite() {
        let _g = setup();

        let mut t = TaskTable::new();
        let f1 = memory::alloc_region(memory::ALLOC_GRANULE).unwrap();
        let r1 = f1.region();
        let rec1 = TaskRecord::new(
            OWNER,
            Box::new(CpuImpl::new_context(ENTRY, r1.base + memory::ALLOC_GRANULE)),
            Kernelstack::new(r1.base, memory::ALLOC_GRANULE),
            f1,
        );
        let id = TaskId::from_raw(7);
        assert_eq!(t.insert(id, rec1), Ok(()));

        let f2 = memory::alloc_region(memory::ALLOC_GRANULE).unwrap();
        let r2 = f2.region();
        let rec2 = TaskRecord::new(
            OWNER,
            Box::new(CpuImpl::new_context(ENTRY, r2.base + memory::ALLOC_GRANULE)),
            Kernelstack::new(r2.base, memory::ALLOC_GRANULE),
            f2,
        );

        assert_eq!(t.insert(id, rec2), Err(TaskError::AlreadyExists));
        assert_eq!(t.len(), 1, "no overwrite");
        let stored = t.get(id).expect("stored");
        assert_eq!(stored.kstack.base, r1.base);
    }

    #[test]
    fn start_requires_task_owner() {
        let _g = setup();

        let mut t = TaskTable::new();
        let id = t.create(OWNER, ENTRY).unwrap();

        assert_eq!(t.start(OTHER_OWNER, id), Err(TaskError::WrongOwner));
        assert_eq!(t.get(id).unwrap().state(), TaskState::Created);

        assert_eq!(t.start(OWNER, id), Ok(()));
        assert_eq!(t.get(id).unwrap().state(), TaskState::Runnable);
    }

    #[test]
    fn has_live_tasks_counts_only_owned_unfinished_tasks() {
        let _g = setup();

        // Given：空表。
        let mut t = TaskTable::new();
        assert!(!t.has_live_tasks(OWNER), "empty table owns nothing");

        // When/Then：Created 算未完成；别人的任务不算我的。
        let created = t.create(OWNER, ENTRY).unwrap();
        assert!(t.has_live_tasks(OWNER), "Created is unfinished");
        assert!(
            !t.has_live_tasks(OTHER_OWNER),
            "foreign owner must not block"
        );

        // When/Then：Exited 是唯一终态；yield 只提交 Runnable，不自然退出。
        t.transition(created, TaskState::Runnable).unwrap();
        assert!(t.has_live_tasks(OWNER), "Runnable is unfinished");
        t.transition(created, TaskState::Running(CpuId(0))).unwrap();
        t.transition(created, TaskState::Exited).unwrap();
        assert!(!t.has_live_tasks(OWNER), "Exited does not block stop");
    }

    // -- Property tests（task 状态机真相，docs/testing.md §2 / §5）------------
    //
    // 对同一张 TaskTable 施加随机长序列的 transition / start，逐操作验证：
    // 1. 合法性精确：transition 成功 <=> (from, to) 属于文档化的合法边
    // 2. 拒绝保真：Err 时观察到的状态不变
    // 3. 接受提交：Ok 时观察到的状态 == to
    // 4. Exited 终态：一旦 Exited，序列后续任何 transition 都不得成功
    // 5. has_live_tasks(owner) == 模型：owner 是否存在 state != Exited 的任务
    // 6. 未创建的 id -> NotFound
    //
    // 模型谓词 is_legal 独立于生产实现、直接镜像文档，使断言是真正的 oracle。

    use proptest::prelude::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Target {
        A,
        B,
        /// 从未 create 过的随机 id（create 只产出 0/1）。
        Unknown(TaskId),
    }

    #[derive(Debug, Clone)]
    enum Op {
        Transition {
            target: Target,
            to: TaskState,
        },
        Start {
            target: Target,
            requester: ComponentId,
        },
    }

    impl Op {
        fn target(&self) -> Target {
            match self {
                Op::Transition { target, .. } | Op::Start { target, .. } => *target,
            }
        }
    }

    /// 文档化状态机的唯一真相（与 `transition` 的 doc comment 逐条对应）：
    /// `Created→Runnable`、`Runnable→Running(_)`、`Running(_)→Runnable`、
    /// `Running(_)→Exited`；其余一律非法（含 Exited 终态）。
    fn is_legal(from: &TaskState, to: &TaskState) -> bool {
        matches!(
            (from, to),
            (TaskState::Created, TaskState::Runnable)
                | (TaskState::Runnable, TaskState::Running(_))
                | (TaskState::Running(_), TaskState::Runnable)
                | (TaskState::Running(_), TaskState::Exited)
        )
    }

    fn state_strategy() -> impl Strategy<Value = TaskState> {
        prop_oneof![
            Just(TaskState::Created),
            Just(TaskState::Runnable),
            Just(TaskState::Running(CpuId(0))),
            Just(TaskState::Blocked),
            Just(TaskState::Exited),
        ]
    }

    fn target_strategy() -> impl Strategy<Value = Target> {
        prop_oneof![
            Just(Target::A),
            Just(Target::B),
            // 合法 create 只产出 0/1，故 [2, 0xffff] 必为不存在的 id。
            (2u32..=0xffff).prop_map(|raw| Target::Unknown(TaskId::from_raw(raw))),
        ]
    }

    fn op_kind_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (target_strategy(), state_strategy())
                .prop_map(|(target, to)| Op::Transition { target, to }),
            (
                target_strategy(),
                prop_oneof![Just(OWNER), Just(OTHER_OWNER)]
            )
                .prop_map(|(target, requester)| Op::Start { target, requester }),
        ]
    }

    /// 序列生成器：随机 transition（随机目标状态）与随机 start（随机请求者）。
    /// 上限压到 80，避免在持有全局堆 GUARD 时放大 proptest 用例开销。
    fn op_seq() -> impl Strategy<Value = Vec<Op>> {
        proptest::collection::vec(op_kind_strategy(), 1..=80)
    }

    /// 被测对象 + 独立模型：两个任务分属不同 owner（A=OWNER，B=OTHER_OWNER）。
    struct Harness {
        table: TaskTable,
        a: TaskId,
        b: TaskId,
        model_a: TaskState,
        model_b: TaskState,
    }

    impl Harness {
        fn new() -> Self {
            let mut table = TaskTable::new();
            let a = table.create(OWNER, ENTRY).expect("create task A");
            let b = table.create(OTHER_OWNER, ENTRY).expect("create task B");
            Self {
                table,
                a,
                b,
                model_a: TaskState::Created,
                model_b: TaskState::Created,
            }
        }

        fn id_of(&self, target: Target) -> TaskId {
            match target {
                Target::A => self.a,
                Target::B => self.b,
                Target::Unknown(id) => id,
            }
        }

        fn owner_of(&self, target: Target) -> ComponentId {
            match target {
                Target::A => OWNER,
                Target::B => OTHER_OWNER,
                Target::Unknown(_) => OWNER,
            }
        }

        fn is_known(target: Target) -> bool {
            matches!(target, Target::A | Target::B)
        }

        fn model_state(&self, target: Target) -> TaskState {
            match target {
                Target::A => self.model_a.clone(),
                Target::B => self.model_b.clone(),
                Target::Unknown(_) => TaskState::Exited,
            }
        }

        fn set_model_state(&mut self, target: Target, state: TaskState) {
            match target {
                Target::A => self.model_a = state,
                Target::B => self.model_b = state,
                Target::Unknown(_) => panic!("unknown target has no model state"),
            }
        }
    }

    /// 模型侧不变量 5 的期望：owner 是否有 state != Exited 的任务。
    fn live_model(h: &Harness, owner: ComponentId) -> bool {
        if owner == OWNER {
            h.model_a != TaskState::Exited
        } else if owner == OTHER_OWNER {
            h.model_b != TaskState::Exited
        } else {
            false
        }
    }

    fn assert_live_matches_model(h: &Harness, owner: ComponentId) {
        assert_eq!(
            h.table.has_live_tasks(owner),
            live_model(h, owner),
            "has_live_tasks(owner={}) drifted from model",
            owner.raw()
        );
    }

    /// 施加一个操作并逐条校验不变量 1–6，随后校验不变量 5。
    fn apply_and_check(h: &mut Harness, op: Op) {
        let target = op.target();
        let id = h.id_of(target);
        let known = Harness::is_known(target);

        // Given：操作前的 Core 真相，先与独立模型交叉核对，保证 oracle 可信。
        let observed_before = h.table.get(id).map(|r| r.state());
        if known {
            assert_eq!(
                observed_before,
                Some(h.model_state(target)),
                "table/model drift before op {op:?}"
            );
        } else {
            assert_eq!(observed_before, None, "unknown id {id} must not exist");
        }

        match op {
            Op::Transition { to, .. } => {
                let result = h.table.transition(id, to.clone());

                if !known {
                    // 6：未创建的 id 只暴露存在性失败，且不产生任何状态。
                    assert_eq!(result, Err(TaskError::NotFound));
                } else {
                    let from = h.model_state(target);
                    let legal = is_legal(&from, &to);

                    // 4：Exited 终态——对任意 to 都没有合法出边。
                    if from == TaskState::Exited {
                        assert!(
                            !legal,
                            "Exited must have no legal outgoing edge (to={to:?})"
                        );
                    }

                    if legal {
                        // 3：接受即提交精确状态。
                        assert_eq!(result, Ok(()), "legal edge {from:?} -> {to:?} must succeed");
                        assert_eq!(
                            h.table.get(id).map(|r| r.state()),
                            Some(to.clone()),
                            "accepted transition must commit `to`"
                        );
                        h.set_model_state(target, to);
                    } else {
                        // 1：每个非法 (from, to) 都必须被显式拒绝。
                        assert_eq!(
                            result,
                            Err(TaskError::InvalidTransition),
                            "illegal edge {from:?} -> {to:?} must be rejected"
                        );
                        // 2：拒绝保真——状态不变。
                        assert_eq!(
                            h.table.get(id).map(|r| r.state()),
                            Some(from),
                            "rejected transition must not change state"
                        );
                    }
                }
            }
            Op::Start { requester, .. } => {
                let result = h.table.start(requester, id);

                if !known {
                    assert_eq!(result, Err(TaskError::NotFound));
                } else if requester != h.owner_of(target) {
                    // 非 owner 不得启动，且不得改变状态。
                    let before = h.model_state(target);
                    assert_eq!(result, Err(TaskError::WrongOwner));
                    assert_eq!(h.table.get(id).map(|r| r.state()), Some(before));
                } else {
                    let from = h.model_state(target);
                    if is_legal(&from, &TaskState::Runnable) {
                        assert_eq!(result, Ok(()));
                        assert_eq!(
                            h.table.get(id).map(|r| r.state()),
                            Some(TaskState::Runnable)
                        );
                        h.set_model_state(target, TaskState::Runnable);
                    } else {
                        assert_eq!(result, Err(TaskError::InvalidTransition));
                        assert_eq!(h.table.get(id).map(|r| r.state()), Some(from));
                    }
                }
            }
        }

        // 5：每步之后，两个 owner 的存活判定都必须与模型一致。
        assert_live_matches_model(h, OWNER);
        assert_live_matches_model(h, OTHER_OWNER);
    }

    proptest! {
        #[test]
        fn random_transition_sequence_preserves_task_truth(ops in op_seq()) {
            // Given：全局堆一次初始化 + 进程级互斥（create 会分配真实 kstack region）。
            let _g = setup();
            let mut h = Harness::new();

            // When：施加随机长序列。
            for op in ops {
                apply_and_check(&mut h, op);
            }

            // Then（收尾）：归还 kstack region，确认清理路径也成立。
            h.table.remove(h.a).expect("remove task A");
            h.table.remove(h.b).expect("remove task B");
            prop_assert_eq!(h.table.len(), 0);
        }
    }

    #[test]
    fn is_legal_matches_documented_edges_exhaustively() {
        // 防呆：独立 oracle 本身必须恰好等于文档化的 4 条边（5×5 穷举）。
        let states = [
            TaskState::Created,
            TaskState::Runnable,
            TaskState::Running(CpuId(0)),
            TaskState::Blocked,
            TaskState::Exited,
        ];
        let documented = [
            (TaskState::Created, TaskState::Runnable),
            (TaskState::Runnable, TaskState::Running(CpuId(0))),
            (TaskState::Running(CpuId(0)), TaskState::Runnable),
            (TaskState::Running(CpuId(0)), TaskState::Exited),
        ];
        for from in &states {
            for to in &states {
                let expected = documented.iter().any(|(f, t)| f == from && t == to);
                assert_eq!(
                    is_legal(from, to),
                    expected,
                    "is_legal({from:?}, {to:?}) must equal documented edge set"
                );
            }
        }
    }

    /// 性能基线（`make bench`）：**task 数量增长时的趋势**。
    ///
    /// 先证明 O(N) 是不是真问题，再决定加不加索引（与 handle scaling 同一模式）。
    #[test]
    #[ignore = "性能基线：make bench 手动跑"]
    fn bench_task_scaling() {
        let _heap = crate::memory::test_support::GUARD.lock();
        crate::memory::test_support::ensure_init();

        crate::bench::report_environment();
        const SIZES: [(usize, &str); 3] = [
            (1, "task.lookup.n1"),
            (32, "task.lookup.n32"),
            (256, "task.lookup.n256"),
        ];
        for (count, name) in SIZES {
            let mut table = TaskTable::new();
            let mut ids = alloc::vec::Vec::new();
            for _ in 0..count {
                let id = table.create(OWNER, 0x8000_0000).unwrap();
                table.transition(id, TaskState::Runnable).unwrap();
                ids.push(id);
            }
            let probe = ids[count / 2];
            let mut bench = crate::bench::Bench::new(name);
            bench.run(100, || table.get(probe).is_some());
            bench.finish().report();
            for id in ids {
                table.remove(id).unwrap();
            }
        }
    }

    // -- 并发探索（docs/testing.md §2）：全局 TASK_TABLE 多线程压力 ---------------
    //
    // 守卫分析：**所有会改动全局任务表的测试都持有 `memory::test_support::GUARD`**
    // （task/mod.rs 与本文件的 setup、sched.rs 绝大多数用例、exit.rs / failure.rs /
    // inspector.rs 的 setup、export.rs 的合格用例——见各文件）。本用例主线程在
    // spawn 之前就取下同一把 GUARD（外加 `ensure_init`），把其它测试整体排除；
    // worker 线程绝不再取 GUARD（否则与主线程自锁），互斥交给 Core 自己的
    // `TASK_TABLE: spin::Mutex`。
    //
    // 少数只读用例（sched::commit_gate_fails_closed_for_unknown_task 查固定幽灵 id、
    // handle::context 解析 ambient 身份）不持 GUARD，但它们只读固定值，且本用例
    // 的 TaskId 由全局计数器唯一分配、跑完即 remove，不会与它们相撞。
    //
    // 加锁纪律：每个操作只取一次全局任务表锁（create / get / transition / remove
    // 各自独立作用域，锁在语句结束即释放），绝不并发持有两把表锁。

    /// 在全局任务表上跑完一次合法生命周期：
    /// `create → Created → Runnable → Running(CpuId(0)) → Runnable → Running → Exited → remove`。
    ///
    /// 每一步都重新取锁并断言"接受即提交精确状态"；任何一步失败（包括因其它线程
    /// 干扰拿到 `NotFound` / `WrongOwner` / `InvalidTransition`）都会 panic，
    /// 由 join 处的 `expect("worker thread panicked")` 传播到测试。
    fn global_lifecycle(owner: ComponentId, entry: usize) -> TaskId {
        let table = crate::task::get_task_table();

        let id = table.lock().create(owner, entry).expect("create");

        let state = table.lock().get(id).expect("created task present").state();
        assert_eq!(state, TaskState::Created, "create must commit Created");

        table
            .lock()
            .transition(id, TaskState::Runnable)
            .expect("Created -> Runnable");
        assert_eq!(
            table.lock().get(id).expect("present").state(),
            TaskState::Runnable,
            "accepted transition must commit Runnable"
        );

        table
            .lock()
            .transition(id, TaskState::Running(CpuId(0)))
            .expect("Runnable -> Running");
        assert_eq!(
            table.lock().get(id).expect("present").state(),
            TaskState::Running(CpuId(0)),
            "accepted transition must commit Running(CpuId(0))"
        );

        table
            .lock()
            .transition(id, TaskState::Runnable)
            .expect("Running -> Runnable");
        assert_eq!(
            table.lock().get(id).expect("present").state(),
            TaskState::Runnable,
            "accepted transition must commit Runnable"
        );

        table
            .lock()
            .transition(id, TaskState::Running(CpuId(0)))
            .expect("Runnable -> Running");
        assert_eq!(
            table.lock().get(id).expect("present").state(),
            TaskState::Running(CpuId(0)),
            "accepted transition must commit Running(CpuId(0))"
        );

        table
            .lock()
            .transition(id, TaskState::Exited)
            .expect("Running -> Exited");
        assert_eq!(
            table.lock().get(id).expect("present").state(),
            TaskState::Exited,
            "accepted transition must commit Exited"
        );

        let record = table.lock().remove(id).expect("remove");
        assert_eq!(record.owner(), owner, "owner must round-trip");
        assert_eq!(
            record.state(),
            TaskState::Exited,
            "removed record must still be Exited"
        );
        id
    }

    /// 4 线程同起跑，在同一张全局任务表上各跑 200 次完整生命周期：
    /// id 跨线程唯一、每次转换精确提交、每个线程只看到 `Ok`、结束后无任务泄漏。
    #[test]
    fn concurrent_lifecycles_on_global_task_table_keep_ids_unique_and_states_exact() {
        // Given：进程级任务表 + 全局堆一次初始化；GUARD 在 spawn 之前就持有，
        // 之后所有会改动全局任务表的测试都被排除在外。
        crate::task::init();
        let _guard = test_support::GUARD.lock();
        test_support::ensure_init();

        const THREADS: usize = 4;
        const OPS: usize = 200;
        let before = crate::task::get_task_table().lock().len();
        let barrier = std::sync::Barrier::new(THREADS);

        // When：所有线程在 barrier 上对齐后各自反复跑完整生命周期。
        let per_thread: Vec<Vec<u32>> = std::thread::scope(|scope| {
            let mut workers = Vec::with_capacity(THREADS);
            for t in 0..THREADS {
                let barrier = &barrier;
                workers.push(scope.spawn(move || {
                    let owner = ComponentId::from_raw(0x4000 + t as u32);
                    barrier.wait();
                    let mut ids = Vec::with_capacity(OPS);
                    for _ in 0..OPS {
                        ids.push(global_lifecycle(owner, ENTRY).raw());
                    }
                    ids
                }));
            }
            workers
                .into_iter()
                .map(|worker| worker.join().expect("worker thread panicked"))
                .collect()
        });

        // Then：4×200 个 id 全部返回，且跨线程两两不同。
        let all: Vec<u32> = per_thread.iter().flatten().copied().collect();
        assert_eq!(all.len(), THREADS * OPS, "every op must return an id");
        let mut unique = all.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            all.len(),
            "task ids must be unique across threads"
        );

        // Then：每个生命周期都在 worker 内 remove 过——全局表长度回到测试前，
        // 没有泄漏的任务（我们在持 GUARD，期间无其它测试能改动全表）。
        assert_eq!(
            crate::task::get_task_table().lock().len(),
            before,
            "every created task must be removed again"
        );
    }
}
