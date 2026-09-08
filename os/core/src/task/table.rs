//! 任务真相存储：Core 唯一的任务清单（id → TaskRecord）。

use crate::memory;
use crate::task::error::TaskError;
use crate::task::id::TaskId;
use crate::task::kstack::Kernelstack;
use crate::task::record::TaskRecord;
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

    /// 唯二创建入口（public）：分配 id + 登记 record。
    pub fn create(&mut self, entry: usize) -> Result<TaskId, TaskError> {
        let id = self.alloc();
        let memory = memory::alloc_region(memory::PAGE_SIZE).map_err(|_| TaskError::NoMemory)?;
        let region = memory.region();
        let kstack = Kernelstack::new(region.base, memory::PAGE_SIZE);
        let context = CpuImpl::new_context(entry, kstack.base + kstack.size);
        let record = TaskRecord::new(Box::new(context), kstack, memory);
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
        let id = t.create(ENTRY).expect("create");
        assert_eq!(id.raw(), 0, "first id is 0");

        assert_eq!(t.len(), 1);
        assert!(t.contains(id));
        let rec = t.get(id).expect("get after create");
        assert_eq!(rec.state, TaskState::Created);
        assert!(
            rec.kstack.base.is_multiple_of(memory::PAGE_SIZE),
            "kstack base page-aligned"
        );
        assert_eq!(rec.kstack.size, memory::PAGE_SIZE);
    }

    #[test]
    fn get_mut_mutation_is_visible() {
        let _g = setup();

        let mut t = TaskTable::new();
        let id = t.create(ENTRY).unwrap();
        assert!(matches!(t.get(id).unwrap().state, TaskState::Created));

        t.get_mut(id).unwrap().state = TaskState::Running(CpuId(0));
        assert!(matches!(t.get(id).unwrap().state, TaskState::Running(_)));
    }

    #[test]
    fn sequential_ids_are_unique_and_iterated_in_order() {
        let _g = setup();

        let mut t = TaskTable::new();
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(t.create(ENTRY).expect("create").raw());
        }
        assert_eq!(ids, [0, 1, 2], "sequential unique ids");

        let iter: Vec<u32> = t.iter().map(|(id, _)| id.raw()).collect();
        assert_eq!(iter, [0, 1, 2], "BTreeMap iterates in id order");
    }

    #[test]
    fn remove_returns_record_and_empties() {
        let _g = setup();

        let mut t = TaskTable::new();
        let id = t.create(ENTRY).unwrap();
        let rec = t.remove(id).expect("remove");
        assert_eq!(rec.kstack.size, memory::PAGE_SIZE);
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
        while let Ok(lease) = memory::alloc_region(memory::PAGE_SIZE) {
            held.push(lease);
        }
        assert!(matches!(t.create(ENTRY), Err(TaskError::NoMemory)));
        assert!(t.is_empty(), "failed create must not register");

        drop(held);
    }

    #[test]
    fn insert_rejects_duplicate_without_overwrite() {
        let _g = setup();

        let mut t = TaskTable::new();
        let f1 = memory::alloc_region(memory::PAGE_SIZE).unwrap();
        let r1 = f1.region();
        let rec1 = TaskRecord::new(
            Box::new(CpuImpl::new_context(
                ENTRY,
                r1.base + memory::PAGE_SIZE,
            )),
            Kernelstack::new(r1.base, memory::PAGE_SIZE),
            f1,
        );
        let id = TaskId::from_raw(7);
        assert_eq!(t.insert(id, rec1), Ok(()));

        let f2 = memory::alloc_region(memory::PAGE_SIZE).unwrap();
        let r2 = f2.region();
        let rec2 = TaskRecord::new(
            Box::new(CpuImpl::new_context(
                ENTRY,
                r2.base + memory::PAGE_SIZE,
            )),
            Kernelstack::new(r2.base, memory::PAGE_SIZE),
            f2,
        );

        assert_eq!(t.insert(id, rec2), Err(TaskError::AlreadyExists));
        assert_eq!(t.len(), 1, "no overwrite");
        let stored = t.get(id).expect("stored");
        assert_eq!(stored.kstack.base, r1.base);
    }
}
