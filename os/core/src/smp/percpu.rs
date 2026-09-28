//! `PerCpu<T>`：按逻辑 `CpuId` 索引的**普通**存储。
//!
//! 刻意保持“普通”：不是魔法同步原语。
//!
//! - **不**实现无条件 `Send` / `Sync`；
//! - **不**提供 `get_mut(&self)`（构造期独占访问用 `get_mut(&mut self)`）；
//! - 发布地址后**不得**再扩容或移动（`Box<[T]>` 保证堆上稳定）。
//!
//! `CpuId` 是稠密下标，因此 `get` 是 O(1)。成员级别的并发访问策略由持有者
//! 决定（例如每个 slot 包一层 `Mutex`），不在本类型里预设。

use crate::machine::CpuId;
use alloc::boxed::Box;
use alloc::vec::Vec;

/// [`PerCpu::new`] 的失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerCpuError {
    /// `count == 0`：至少一个 CPU。
    InvalidCount,
    /// 分配失败（宿主/内核分配器返回错误）。
    AllocationFailed,
}

/// 按 `CpuId` 索引的定长存储。
pub struct PerCpu<T> {
    slots: Box<[T]>,
}

impl<T> PerCpu<T> {
    /// 为 `count` 个逻辑 CPU 构造，逐个调用 `init(cpu)` 初始化。
    ///
    /// 分配失败时返回 [`PerCpuError::AllocationFailed`]，不 panic。
    pub fn new(count: usize, mut init: impl FnMut(CpuId) -> T) -> Result<Self, PerCpuError> {
        if count == 0 {
            return Err(PerCpuError::InvalidCount);
        }
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| PerCpuError::AllocationFailed)?;
        for index in 0..count {
            slots.push(init(CpuId::from_raw(index)));
        }
        debug_assert_eq!(slots.len(), count);
        Ok(Self {
            slots: slots.into_boxed_slice(),
        })
    }

    /// 只读访问某个 CPU 的槽位；越界返回 `None`。
    pub fn get(&self, cpu: CpuId) -> Option<&T> {
        self.slots.get(cpu.raw())
    }

    /// 可变访问；**仅构造期**使用（需要 `&mut self`，故不可能与已发布的只读
    /// 引用并存）。
    pub fn get_mut(&mut self, cpu: CpuId) -> Option<&mut T> {
        self.slots.get_mut(cpu.raw())
    }

    /// 槽位数量（= 构造时的 `count`）。
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// 是否为空（正常构造下恒为 `false`）。
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// 按逻辑 CPU 升序遍历 `(CpuId, &T)`。
    pub fn iter(&self) -> impl Iterator<Item = (CpuId, &T)> {
        self.slots
            .iter()
            .enumerate()
            .map(|(index, value)| (CpuId::from_raw(index), value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_initializes_each_slot_with_its_cpu_id() {
        let per_cpu = PerCpu::new(4, |cpu| cpu.raw() * 10).unwrap();
        assert_eq!(per_cpu.len(), 4);
        for index in 0..4 {
            assert_eq!(per_cpu.get(CpuId::from_raw(index)), Some(&(index * 10)));
        }
        assert_eq!(per_cpu.get(CpuId::from_raw(4)), None);
    }

    #[test]
    fn zero_count_is_rejected() {
        assert_eq!(
            PerCpu::<u8>::new(0, |_| 0).err(),
            Some(PerCpuError::InvalidCount)
        );
    }

    #[test]
    fn get_mut_touches_only_the_target_slot() {
        let mut per_cpu = PerCpu::new(3, |_| 0u32).unwrap();
        *per_cpu.get_mut(CpuId::from_raw(1)).unwrap() = 7;
        assert_eq!(per_cpu.get(CpuId::from_raw(0)), Some(&0));
        assert_eq!(per_cpu.get(CpuId::from_raw(1)), Some(&7));
        assert_eq!(per_cpu.get(CpuId::from_raw(2)), Some(&0));
    }

    #[test]
    fn iteration_is_ascending_by_cpu_id() {
        let per_cpu = PerCpu::new(3, |cpu| cpu.raw()).unwrap();
        let seen: alloc::vec::Vec<usize> = per_cpu.iter().map(|(cpu, _)| cpu.raw()).collect();
        assert_eq!(seen, alloc::vec![0, 1, 2]);
    }
}
