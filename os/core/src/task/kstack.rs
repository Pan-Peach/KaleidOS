//! 任务内核栈真相：范围 + 归属。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kernelstack {
    pub base: usize,
    pub size: usize,
}

impl Kernelstack {
    pub fn new(base: usize, size: usize) -> Self {
        Self { base, size }
    }
}
