//! `CpuMask`：逻辑 CPU 集合。
//!
//! 上界是编译期容量 [`crate::machine::MAX_CPUS`]（= discovery 数组容量），
//! 不是运行时数量；越界索引是**错误**，不是静默截断（Core 不发明“最近似”）。

use crate::machine::{CpuId, MAX_CPUS};

/// 每个 `usize` 承载的位数。
const BITS_PER_WORD: usize = usize::BITS as usize;
/// 覆盖 `MAX_CPUS` 需要的字数。
const WORDS: usize = MAX_CPUS.div_ceil(BITS_PER_WORD);

/// 索引越界（逻辑 CPU 超出编译期容量）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuIndexError {
    OutOfRange,
}

/// 逻辑 CPU 的位集。`CpuId` 是稠密下标，因此可直接寻址。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CpuMask {
    words: [usize; WORDS],
}

impl CpuMask {
    /// 空集合。
    pub const fn empty() -> Self {
        Self { words: [0; WORDS] }
    }

    /// 加入一个 CPU；越界返回 [`CpuIndexError::OutOfRange`]。
    pub fn insert(&mut self, cpu: CpuId) -> Result<(), CpuIndexError> {
        let index = cpu.raw();
        if index >= MAX_CPUS {
            return Err(CpuIndexError::OutOfRange);
        }
        self.words[index / BITS_PER_WORD] |= 1 << (index % BITS_PER_WORD);
        Ok(())
    }

    /// 移除一个 CPU；越界返回 [`CpuIndexError::OutOfRange`]。
    pub fn remove(&mut self, cpu: CpuId) -> Result<(), CpuIndexError> {
        let index = cpu.raw();
        if index >= MAX_CPUS {
            return Err(CpuIndexError::OutOfRange);
        }
        self.words[index / BITS_PER_WORD] &= !(1 << (index % BITS_PER_WORD));
        Ok(())
    }

    /// 是否包含该 CPU；越界视为 `false`（查询不报错，避免热路径分支）。
    pub fn contains(&self, cpu: CpuId) -> bool {
        let index = cpu.raw();
        index < MAX_CPUS
            && (self.words[index / BITS_PER_WORD] & (1 << (index % BITS_PER_WORD))) != 0
    }

    /// 已置位的 CPU 数量。
    pub fn count(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// 集合是否为空。
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|word| *word == 0)
    }

    /// 升序遍历集合中的 CPU。
    pub fn iter(&self) -> CpuMaskIter<'_> {
        CpuMaskIter {
            mask: self,
            next: 0,
        }
    }
}

impl<'a> IntoIterator for &'a CpuMask {
    type Item = CpuId;
    type IntoIter = CpuMaskIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// [`CpuMask`] 的升序迭代器。
pub struct CpuMaskIter<'a> {
    mask: &'a CpuMask,
    next: usize,
}

impl Iterator for CpuMaskIter<'_> {
    type Item = CpuId;

    fn next(&mut self) -> Option<CpuId> {
        while self.next < MAX_CPUS {
            let candidate = CpuId::from_raw(self.next);
            self.next += 1;
            if self.mask.contains(candidate) {
                return Some(candidate);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_mask_contains_nothing() {
        let mask = CpuMask::empty();
        assert!(mask.is_empty());
        assert_eq!(mask.count(), 0);
        assert!(!mask.contains(CpuId::from_raw(0)));
        assert_eq!(mask.iter().count(), 0);
    }

    #[test]
    fn insert_remove_and_contains_round_trip() {
        let mut mask = CpuMask::empty();
        mask.insert(CpuId::from_raw(0)).unwrap();
        mask.insert(CpuId::from_raw(3)).unwrap();
        assert!(mask.contains(CpuId::from_raw(0)));
        assert!(mask.contains(CpuId::from_raw(3)));
        assert!(!mask.contains(CpuId::from_raw(1)));
        assert_eq!(mask.count(), 2);

        mask.remove(CpuId::from_raw(0)).unwrap();
        assert!(!mask.contains(CpuId::from_raw(0)));
        assert_eq!(mask.count(), 1);
    }

    #[test]
    fn out_of_range_is_a_rejected_error_not_truncation() {
        let mut mask = CpuMask::empty();
        let out_of_range = CpuId::from_raw(MAX_CPUS);
        assert_eq!(mask.insert(out_of_range), Err(CpuIndexError::OutOfRange));
        assert_eq!(mask.remove(out_of_range), Err(CpuIndexError::OutOfRange));
        assert!(!mask.contains(out_of_range));
        assert!(mask.is_empty());
    }

    #[test]
    fn iteration_is_ascending_and_skips_gaps() {
        let mut mask = CpuMask::empty();
        for cpu in [2usize, 0, 3] {
            mask.insert(CpuId::from_raw(cpu)).unwrap();
        }
        let seen: alloc::vec::Vec<usize> = mask.iter().map(|cpu| cpu.raw()).collect();
        assert_eq!(seen, alloc::vec![0, 2, 3]);
    }
}
