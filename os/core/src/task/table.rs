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
}
