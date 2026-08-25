//! 任务真相存储：Core 唯一的任务清单（id → TaskRecord）。

use crate::memory;
use crate::task::error::TaskError;
use crate::task::id::TaskId;
use crate::task::kstack::Kernelstack;
use crate::task::record::TaskRecord;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use arch::{Arch, ArchImpl};
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
        let frame = memory::alloc_frame().map_err(|_| TaskError::NoMemory)?;
        let kstack = Kernelstack::new(frame.start_pa(), memory::FRAME_SIZE);
        let context = ArchImpl::new_context(entry, kstack.base + kstack.size);
        let record = TaskRecord::new(Box::new(context), kstack);
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
