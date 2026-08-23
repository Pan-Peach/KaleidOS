//! 帧真相数据库：FrameDatabase（帧 → 状态映射）与 MachineInfo → 帧数据库的 adapter。
//!
//! 数据模型（FrameId/FrameState/Owner/FrameMeta/FrameError）在 `mod.rs`。
//! 本文件只做：帧号 → 状态的一一对应存储（O(1)）与 init 的 validate/merge/commit。
//! **切片式（无 const N）**：帧状态数组本体由调用方持有，`FrameDatabase` 只借用 `&mut [FrameMeta]`
//! —— 无论内存多大，数据库结构体本身只有"指针+长度"，不会把栈爆掉。

use super::*;
use crate::machine::MemoryRegion;
/// 一段连续 RAM 的帧状态数据库：帧号 → 状态，一一对应，O(1) 查询。
/// `frames` 借用自调用方（切片式）；`frame_count` 即切片长度。
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct FrameDatabase<'a> {
    base_frame: FrameId,
    frames: &'a mut [FrameMeta],
}

// ⑥ impl FrameId
impl FrameId {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    /// 该物理地址所在的帧号。
    pub const fn from_pa(pa: usize) -> Self {
        Self(pa as u64 / FRAME_SIZE as u64)
    }

    /// 该帧的起始物理地址（调试用）。
    pub const fn start_pa(self) -> usize {
        self.0 as usize * FRAME_SIZE
    }
}

// ⑦ impl FrameDatabase
impl<'a> FrameDatabase<'a> {
    pub fn new(base_frame: FrameId, frames: &'a mut [FrameMeta]) -> Self {
        Self { base_frame, frames }
    }

    /// id >= base_frame 且 offset < frame_count。
    pub fn contains(&self, frame_id: FrameId) -> bool {
        frame_id.0 >= self.base_frame.0
            && frame_id.0 - self.base_frame.0 < self.frames.len() as u64
    }

    pub fn state(&self, frame_id: FrameId) -> Option<FrameState> {
        if self.contains(frame_id) {
            let index = (frame_id.0 - self.base_frame.0) as usize;
            Some(self.frames[index].state)
        } else {
            None
        }
    }

    /// 保留连续帧（Core 自留：内核镜像/DTB 等由上层 adapter 调用）。
    /// 先全量验证，再全量写入 —— 失败时不得部分提交。
    pub fn reserve_range(&mut self, start: FrameId, count: usize) -> Result<(), FrameError> {
        if count == 0 {
            return Ok(());
        }
        let first_index = start
            .0
            .checked_sub(self.base_frame.0)
            .ok_or(FrameError::OutOfRange)?;
        let last_index = first_index
            .checked_add(count as u64 - 1)
            .ok_or(FrameError::OutOfRange)?;
        if last_index >= self.frames.len() as u64 {
            return Err(FrameError::OutOfRange);
        }
        // validate
        for i in 0..count {
            if self.frames[(first_index + i as u64) as usize].state != FrameState::Free {
                return Err(FrameError::NotFree);
            }
        }
        // commit
        for i in 0..count {
            self.frames[(first_index + i as u64) as usize].state = FrameState::Reserved;
        }
        Ok(())
    }

    /// Free → Owned(owner)。返回 ()：Handle 语义（generation/token/slot）留给 handle.rs 阶段。
    pub fn own_frame(&mut self, frame_id: FrameId, owner: Owner) -> Result<(), FrameError> {
        if !self.contains(frame_id) {
            return Err(FrameError::OutOfRange);
        }
        let index = (frame_id.0 - self.base_frame.0) as usize;
        match self.frames[index].state {
            FrameState::Free => {
                self.frames[index].state = FrameState::Owned(owner);
                Ok(())
            }
            FrameState::Reserved => Err(FrameError::NotFree),
            FrameState::Owned(_) => Err(FrameError::AlreadyOwned),
        }
    }
}



// ⑥ adapter：MachineInfo world → FrameDatabase world（core::init 调用）
/// 机器发现 → 帧真相的 adapter。
/// - `regions`: 可用物理 RAM 区间（MachineInfo.memory_regions）
/// - `reserved`: 需保留的区间（ELF image range / 固件驻留，由 bootstrap/adapter 给出）
/// - `frames`: 帧数组本体（切片式，由调用方持有）
///
/// 流程：
/// 1. validate：对齐 / 非空（不满足即报错——4K 边界不齐帧没法算）；
/// 2. merge：重叠 regions 合并成连续区间（发现阶段提案冗余很正常，不报错）；
/// 3. commit：为每个 region 建 FrameDatabase，reserved 落在 RAM 内的部分标 Reserved
///    （与 RAM 无交集则忽略，如 MMIO）。
pub fn init(
    regions: &[MemoryRegion],
    reserved: &[MemoryRegion],
    frames: &mut [FrameMeta],
) -> Result<(), &'static str> {
    if regions.is_empty() {
        return Err("no memory regions");
    }
    // validate：对齐 / 非空
    for r in regions.iter().chain(reserved) {
        if r.base % FRAME_SIZE != 0 || r.size % FRAME_SIZE != 0 {
            return Err("region not frame-aligned");
        }
        if r.size == 0 {
            return Err("empty region");
        }
    }
    // merge：重叠区间合并（memblock 风格，不报错）
    let mut merged: [MemoryRegion; 16] = [MemoryRegion { base: 0, size: 0 }; 16];
    let merged_count = merge_overlapping(regions, &mut merged);
    // commit：每个 region 建表；帧数组按 region 顺序滑动切分
    let mut remaining = frames;
    for (ri, region) in merged[..merged_count].iter().enumerate() {
        let frame_count = region.size / FRAME_SIZE;
        if frame_count > remaining.len() {
            return Err("frame array too small for region");
        }
        let (region_frames, rest) = remaining.split_at_mut(frame_count);
        remaining = rest;
        let mut db = FrameDatabase::new(FrameId::from_pa(region.base), region_frames);
        if ri != 0 {
            continue; // 多 region 简化：reserved 只处理第一个（QEMU 单 region）
        }
        for r in reserved {
            let start = r.base.max(region.base);
            let end = (r.base + r.size).min(region.base + region.size);
            if start >= end {
                continue; // 与 RAM 无交集（如 MMIO）→ 忽略
            }
            let first = start / FRAME_SIZE;
            let count = (end + FRAME_SIZE - 1) / FRAME_SIZE - first;
            db.reserve_range(FrameId::from_raw(first as u64), count)
                .map_err(|_| "reserved range invalid")?;
        }
    }
    Ok(())
}

/// 把 `regions` 中重叠/相邻的区间合并为不相交列表（写进 `out`，返回数量）。
/// 合并规则：新区间起点 <= 已有区间终点 → 并入（取最大 extent）。
pub fn merge_overlapping(regions: &[MemoryRegion], out: &mut [MemoryRegion]) -> usize {
    let mut sorted: [MemoryRegion; 16] = [MemoryRegion { base: 0, size: 0 }; 16];
    let n = regions.len().min(sorted.len());
    sorted[..n].copy_from_slice(&regions[..n]);
    // 按 base 升序（简单插入排序，region 数极少）
    for i in 1..n {
        let mut j = i;
        while j > 0 && sorted[j - 1].base > sorted[j].base {
            sorted.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut out_count = 0usize;
    for r in sorted[..n].iter() {
        if out_count == 0 {
            out[0] = *r;
            out_count = 1;
            continue;
        }
        let last = &mut out[out_count - 1];
        let last_end = last.base + last.size;
        let cur_end = r.base + r.size;
        if r.base <= last_end {
            // 重叠或相邻 → 并入
            if cur_end > last_end {
                last.size = cur_end - last.base;
            }
        } else {
            out[out_count] = *r;
            out_count += 1;
        }
    }
    out_count
}

// ⑧ tests
#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::MemoryRegion;

    const BASE: u64 = 0x80000;
    const N: usize = 16;

    fn frame_array() -> [FrameMeta; N] {
        [FrameMeta::new(FrameState::Free); N]
    }

    fn db(frames: &mut [FrameMeta; N]) -> FrameDatabase<'_> {
        FrameDatabase::new(FrameId::from_raw(BASE), &mut frames[..])
    }

    #[test]
    fn ids_with_same_raw_are_equal() {
        assert_eq!(FrameId::from_raw(100), FrameId::from_raw(100));
    }

    #[test]
    fn raw_roundtrip() {
        assert_eq!(FrameId::from_raw(u64::MAX).raw(), u64::MAX);
    }

    #[test]
    fn from_pa_maps_to_frame() {
        assert_eq!(FrameId::from_pa(0x80204_000).raw(), 0x80204);
        assert_eq!(FrameId::from_pa(0x80204_fff).raw(), 0x80204);
        assert_eq!(FrameId::from_pa(0x80205_000).raw(), 0x80205);
    }

    #[test]
    fn start_pa_roundtrips() {
        assert_eq!(FrameId::from_raw(0x80204).start_pa(), 0x80204_000);
    }

    #[test]
    fn lower_than_base_frame_is_missing() {
        let mut f = frame_array();
        let d = db(&mut f);
        assert_eq!(d.state(FrameId::from_raw(BASE - 1)), None);
    }

    #[test]
    fn first_and_last_frame_are_present() {
        let mut f = frame_array();
        let d = db(&mut f);
        assert_eq!(d.state(FrameId::from_raw(BASE)), Some(FrameState::Free));
        assert_eq!(
            d.state(FrameId::from_raw(BASE + N as u64 - 1)),
            Some(FrameState::Free)
        );
    }

    #[test]
    fn after_end_frame_is_missing() {
        let mut f = frame_array();
        let d = db(&mut f);
        assert_eq!(d.state(FrameId::from_raw(BASE + N as u64)), None);
    }

    #[test]
    fn all_frames_initially_free() {
        let mut f = frame_array();
        let d = db(&mut f);
        for i in 0..N {
            assert_eq!(d.state(FrameId::from_raw(BASE + i as u64)), Some(FrameState::Free));
        }
    }

    #[test]
    fn reserve_middle_two_frames() {
        let mut f = frame_array();
        let mut d = db(&mut f);
        d.reserve_range(FrameId::from_raw(BASE + 2), 2).unwrap();
        assert_eq!(d.state(FrameId::from_raw(BASE + 2)), Some(FrameState::Reserved));
        assert_eq!(d.state(FrameId::from_raw(BASE + 3)), Some(FrameState::Reserved));
        assert_eq!(d.state(FrameId::from_raw(BASE + 1)), Some(FrameState::Free));
    }

    #[test]
    fn failed_reserve_does_not_partially_commit() {
        let mut f = frame_array();
        let mut d = db(&mut f);
        d.own_frame(FrameId::from_raw(BASE + 3), Owner::Core).unwrap();
        let err = d.reserve_range(FrameId::from_raw(BASE + 1), 4);
        assert_eq!(err, Err(FrameError::NotFree));
        assert_eq!(d.state(FrameId::from_raw(BASE + 1)), Some(FrameState::Free));
        assert_eq!(d.state(FrameId::from_raw(BASE + 2)), Some(FrameState::Free));
    }

    #[test]
    fn reserve_out_of_range() {
        let mut f = frame_array();
        let mut d = db(&mut f);
        assert_eq!(
            d.reserve_range(FrameId::from_raw(BASE + N as u64 - 1), 2),
            Err(FrameError::OutOfRange)
        );
    }

    #[test]
    fn reserve_overflow_is_err_not_panic() {
        let mut f = frame_array();
        let mut d = db(&mut f);
        assert_eq!(
            d.reserve_range(FrameId::from_raw(u64::MAX - 1), 2),
            Err(FrameError::OutOfRange)
        );
    }

    #[test]
    fn own_free_frame_succeeds() {
        let mut f = frame_array();
        let mut d = db(&mut f);
        d.own_frame(FrameId::from_raw(BASE + 5), Owner::Core).unwrap();
        assert_eq!(
            d.state(FrameId::from_raw(BASE + 5)),
            Some(FrameState::Owned(Owner::Core))
        );
    }

    #[test]
    fn own_reserved_frame_fails() {
        let mut f = frame_array();
        let mut d = db(&mut f);
        d.reserve_range(FrameId::from_raw(BASE + 5), 1).unwrap();
        assert_eq!(
            d.own_frame(FrameId::from_raw(BASE + 5), Owner::Core),
            Err(FrameError::NotFree)
        );
    }

    #[test]
    fn own_owned_frame_fails() {
        let mut f = frame_array();
        let mut d = db(&mut f);
        d.own_frame(FrameId::from_raw(BASE + 5), Owner::Core).unwrap();
        assert_eq!(
            d.own_frame(FrameId::from_raw(BASE + 5), Owner::Component(
                crate::component::ComponentId::from_raw(1)
            )),
            Err(FrameError::AlreadyOwned)
        );
    }

    #[test]
    fn own_out_of_range_fails_not_panics() {
        let mut f = frame_array();
        let mut d = db(&mut f);
        assert_eq!(
            d.own_frame(FrameId::from_raw(BASE - 1), Owner::Core),
            Err(FrameError::OutOfRange)
        );
    }

    // --- memory::init adapter 契约 ---

    #[test]
    fn init_marks_elf_range_reserved() {
        let mut f = frame_array();
        let regions = [MemoryRegion { base: 0x8000_0000, size: 0x4000 }]; // 4 帧，适配 N=16
        let reserved = [MemoryRegion { base: 0x8000_1000, size: 0x1000 }];
        init(&regions, &reserved, &mut f).unwrap();
        // ELF 保留区间内：Reserved；区间外：Free
        assert_eq!(
            FrameDatabase::new(FrameId::from_pa(0x8000_0000), &mut f[..])
                .state(FrameId::from_pa(0x8000_1000)),
            Some(FrameState::Reserved)
        );
        assert_eq!(
            FrameDatabase::new(FrameId::from_pa(0x8000_0000), &mut f[..])
                .state(FrameId::from_pa(0x8000_3000)),
            Some(FrameState::Free)
        );
    }

    #[test]
    fn init_rejects_un_aligned_region() {
        let mut f = frame_array();
        let regions = [MemoryRegion { base: 0x8000_0800, size: 0x1000 }]; // base 非帧对齐
        let err = init(&regions, &[], &mut f);
        assert_eq!(err, Err("region not frame-aligned"));
    }

    #[test]
    fn init_merges_overlapping_regions() {
        let mut f = frame_array(); // N=16
        let regions = [
            MemoryRegion { base: 0x8000_0000, size: 0x4000 }, // 4 帧
            MemoryRegion { base: 0x8000_1000, size: 0x6000 }, // 6 帧，与前者重叠 0x1000
        ];
        // 合并后 = [0x80000000, 0x80007000)，共 7 帧，数据库只切前 7 槽
        init(&regions, &[], &mut f).unwrap();
        let db = FrameDatabase::new(FrameId::from_pa(0x8000_0000), &mut f[..7]);
        // 重叠区内地址（0x80001000）有效且 Free —— 合并后无 gap
        assert_eq!(db.state(FrameId::from_pa(0x8000_1000)), Some(FrameState::Free));
        // 合并后末帧（0x80006000）有效；下一帧（0x80007000）出界
        assert_eq!(db.state(FrameId::from_pa(0x8000_6000)), Some(FrameState::Free));
        assert_eq!(db.state(FrameId::from_pa(0x8000_7000)), None);
    }

    #[test]
    fn merge_overlapping_helper_basics() {
        let mut out = [MemoryRegion { base: 0, size: 0 }; 16];
        // 不相交：两个独立区间
        let regions = [
            MemoryRegion { base: 0x8000_0000, size: 0x10000 },
            MemoryRegion { base: 0x9000_0000, size: 0x10000 },
        ];
        assert_eq!(merge_overlapping(&regions, &mut out), 2);
        // 重叠：merge 为一个
        let regions = [
            MemoryRegion { base: 0x8000_0000, size: 0x20000 },
            MemoryRegion { base: 0x8001_0000, size: 0x20000 },
        ];
        assert_eq!(merge_overlapping(&regions, &mut out), 1);
        assert_eq!(out[0].base, 0x8000_0000);
        assert_eq!(out[0].size, 0x30000);
        // 包含（一个完全包住另一个）
        let regions = [
            MemoryRegion { base: 0x8000_0000, size: 0x40000 },
            MemoryRegion { base: 0x8001_0000, size: 0x10000 },
        ];
        assert_eq!(merge_overlapping(&regions, &mut out), 1);
        assert_eq!(out[0].size, 0x40000);
        // 乱序输入
        let regions = [
            MemoryRegion { base: 0x9000_0000, size: 0x10000 },
            MemoryRegion { base: 0x8000_0000, size: 0x10000 },
        ];
        assert_eq!(merge_overlapping(&regions, &mut out), 2);
        assert_eq!(out[0].base, 0x8000_0000);
    }
}
