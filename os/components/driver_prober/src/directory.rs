//! 静态候选目录：`compatible`（opaque 路由键）→ 候选驱动组件名。
//!
//! 这里**只有路由数据**：prober 不解析 `compatible`、不知道任何协议偏移或
//! 设备类型取值。一个组件可以声明多个键；目录里重复出现同一组件时，
//! [`CandidateSet::build`] 会去重（键取并集）。

/// 一条目录项：`compatible` 对 prober 是 opaque key，只按字节相等匹配。
pub struct Candidate {
    pub compatible: &'static [u8],
    pub component: &'static [u8],
}

/// 内置候选目录（纯路由数据）。
pub const CANDIDATES: &[Candidate] = &[Candidate {
    compatible: b"virtio,mmio",
    component: b"virtio_blk",
}];

/// 空 key（定长表的空槽）。
const EMPTY: &[u8] = &[];

/// 静态容量（无 alloc）。
pub const MAX_DRIVERS: usize = 8;
pub const MAX_COMPATIBLES_PER_DRIVER: usize = 4;

/// 去重后的候选集：每个唯一组件一条（首见顺序），带它声明的全部 opaque key。
pub struct CandidateSet {
    count: usize,
    drivers: [&'static [u8]; MAX_DRIVERS],
    compatible_count: [usize; MAX_DRIVERS],
    compatibles: [[&'static [u8]; MAX_COMPATIBLES_PER_DRIVER]; MAX_DRIVERS],
}

impl CandidateSet {
    pub const fn new() -> Self {
        Self {
            count: 0,
            drivers: [EMPTY; MAX_DRIVERS],
            compatible_count: [0; MAX_DRIVERS],
            compatibles: [[EMPTY; MAX_COMPATIBLES_PER_DRIVER]; MAX_DRIVERS],
        }
    }

    /// 从静态目录构建：每个唯一组件一条（首见顺序）；同一组件的键取并集、
    /// 不重复存储。超过静态容量（或键超容量）时丢弃多余项（无 alloc）。
    pub fn build(directory: &[Candidate]) -> Self {
        let mut set = Self::new();
        for candidate in directory {
            let slot = match set.index_of(candidate.component) {
                Some(index) => index,
                None => {
                    if set.count >= MAX_DRIVERS {
                        continue;
                    }
                    let index = set.count;
                    set.drivers[index] = candidate.component;
                    set.count += 1;
                    index
                }
            };
            let used = set.compatible_count[slot];
            let already = set.compatibles[slot][..used].contains(&candidate.compatible);
            if !already && used < MAX_COMPATIBLES_PER_DRIVER {
                set.compatibles[slot][used] = candidate.compatible;
                set.compatible_count[slot] = used + 1;
            }
        }
        set
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 第 `index` 个候选的组件名（`index < len`）。
    pub fn driver(&self, index: usize) -> &'static [u8] {
        self.drivers[index]
    }

    /// 第 `index` 个候选声明的 opaque compatible 键。
    pub fn compatibles(&self, index: usize) -> &[&'static [u8]] {
        &self.compatibles[index][..self.compatible_count[index]]
    }

    /// 组件名 → 候选下标（不存在返回 `None`）。
    pub fn index_of(&self, driver: &[u8]) -> Option<usize> {
        (0..self.count).find(|&index| self.drivers[index] == driver)
    }
}

impl Default for CandidateSet {
    fn default() -> Self {
        Self::new()
    }
}
