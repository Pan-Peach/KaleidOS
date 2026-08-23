//! 物理帧真相（单段连续 RAM 版本）：FrameId 身份、FrameState/Owner 状态、FrameDatabase 数据库。
//!
//! 本模块不知道 FDT / MachineInfo / 多 region —— 只描述"一段连续 RAM"；
//! 机器发现 → 帧数据库的转换（adapter）由上层（core::init）负责。
//! 分配算法（buddy / free list）不在此模块：属于 Allocator Component。
//! Core 职责：先验证（validate）再提交（commit）—— reserve_range 体现两遍写入。

// ① constants
const FRAME_SIZE: usize = 4096; // 4KiB

// ② identity
/// 物理帧身份（M1 词汇表）—— Identity，不是 Authority。
/// 帧的存在性、状态与所有权由 Core 记录；分配器只提议，Core 才 commit。
/// 可被猜测/构造/传递，但"知道 FrameId"≠"有权使用该帧"；
/// 来自 Component/IPC/Wasm 的 ID 必须由 Core 重新验证。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameId(u64);

// ③ truth types
/// 帧状态（作为只读查询结果公开）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameState {
    Free,
    Reserved,
    Owned(Owner),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Owner {
    Component(crate::component::ComponentId), // 组件 ID
    Core,
}

/// Core 内部保存 Resource Truth 的数据库记录（不对外公开）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FrameMeta {
    state: FrameState,
}

// ④ errors
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameError {
    OutOfRange,
    NotFree,
    AlreadyOwned,
    Permission,
}

// ⑤ database
/// 一段连续 RAM 的帧状态数据库：帧号 → 状态，一一对应，O(1) 查询。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameDatabase<const N: usize> {
    base_frame: FrameId,
    frame_count: usize,
    frames: [FrameMeta; N],
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
impl<const N: usize> FrameDatabase<N> {
    pub const fn new(base_frame: FrameId, frame_count: usize) -> Self {
        assert!(frame_count <= N, "frame_count exceeds array size N");
        Self {
            base_frame,
            frame_count,
            frames: [FrameMeta {
                state: FrameState::Free,
            }; N],
        }
    }

    /// id >= base_frame 且 offset < frame_count。
    pub const fn contains(&self, frame_id: FrameId) -> bool {
        frame_id.0 >= self.base_frame.0
            && frame_id.0 - self.base_frame.0 < self.frame_count as u64
    }

    pub const fn state(&self, frame_id: FrameId) -> Option<FrameState> {
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
        if last_index >= self.frame_count as u64 {
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

// ⑧ tests
#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x80000;
    const N: usize = 16;

    fn db() -> FrameDatabase<N> {
        FrameDatabase::new(FrameId::from_raw(BASE), N)
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
        assert_eq!(FrameId::from_pa(0x80204_fff).raw(), 0x80204); // 帧内偏移不进下一帧
        assert_eq!(FrameId::from_pa(0x80205_000).raw(), 0x80205);
    }

    #[test]
    fn start_pa_roundtrips() {
        assert_eq!(FrameId::from_raw(0x80204).start_pa(), 0x80204_000);
    }

    #[test]
    fn lower_than_base_frame_is_missing() {
        assert_eq!(db().state(FrameId::from_raw(BASE - 1)), None);
    }

    #[test]
    fn first_and_last_frame_are_present() {
        assert_eq!(db().state(FrameId::from_raw(BASE)), Some(FrameState::Free));
        assert_eq!(
            db().state(FrameId::from_raw(BASE + N as u64 - 1)),
            Some(FrameState::Free)
        );
    }

    #[test]
    fn after_end_frame_is_missing() {
        assert_eq!(db().state(FrameId::from_raw(BASE + N as u64)), None);
    }

    #[test]
    fn all_frames_initially_free() {
        for i in 0..N {
            assert_eq!(db().state(FrameId::from_raw(BASE + i as u64)), Some(FrameState::Free));
        }
    }

    #[test]
    fn reserve_middle_two_frames() {
        let mut d = db();
        d.reserve_range(FrameId::from_raw(BASE + 2), 2).unwrap();
        assert_eq!(d.state(FrameId::from_raw(BASE + 2)), Some(FrameState::Reserved));
        assert_eq!(d.state(FrameId::from_raw(BASE + 3)), Some(FrameState::Reserved));
        assert_eq!(d.state(FrameId::from_raw(BASE + 1)), Some(FrameState::Free));
    }

    #[test]
    fn failed_reserve_does_not_partially_commit() {
        let mut d = db();
        d.own_frame(FrameId::from_raw(BASE + 3), Owner::Core).unwrap();
        let err = d.reserve_range(FrameId::from_raw(BASE + 1), 4);
        assert_eq!(err, Err(FrameError::NotFree));
        // 前两帧必须保持不变（不部分提交）
        assert_eq!(d.state(FrameId::from_raw(BASE + 1)), Some(FrameState::Free));
        assert_eq!(d.state(FrameId::from_raw(BASE + 2)), Some(FrameState::Free));
    }

    #[test]
    fn reserve_out_of_range() {
        let mut d = db();
        assert_eq!(
            d.reserve_range(FrameId::from_raw(BASE + N as u64 - 1), 2),
            Err(FrameError::OutOfRange)
        );
    }

    #[test]
    fn reserve_overflow_is_err_not_panic() {
        let mut d = db();
        assert_eq!(
            d.reserve_range(FrameId::from_raw(u64::MAX - 1), 2),
            Err(FrameError::OutOfRange)
        );
    }

    #[test]
    fn own_free_frame_succeeds() {
        let mut d = db();
        d.own_frame(FrameId::from_raw(BASE + 5), Owner::Core).unwrap();
        assert_eq!(
            d.state(FrameId::from_raw(BASE + 5)),
            Some(FrameState::Owned(Owner::Core))
        );
    }

    #[test]
    fn own_reserved_frame_fails() {
        let mut d = db();
        d.reserve_range(FrameId::from_raw(BASE + 5), 1).unwrap();
        assert_eq!(
            d.own_frame(FrameId::from_raw(BASE + 5), Owner::Core),
            Err(FrameError::NotFree)
        );
    }

    #[test]
    fn own_owned_frame_fails() {
        let mut d = db();
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
        let mut d = db();
        assert_eq!(
            d.own_frame(FrameId::from_raw(BASE - 1), Owner::Core),
            Err(FrameError::OutOfRange)
        );
    }
}
