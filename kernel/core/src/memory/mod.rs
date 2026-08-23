//! 物理帧真相：FrameId 身份、FrameState/Owner 状态、FrameMeta 记录、FrameError。
//!
//! 本模块不知道 FDT / MachineInfo / 多 region —— 只描述"一段连续 RAM"；
//! 机器发现 → 帧数据库的转换（adapter）由 `frame_db::init` 负责。
//! 分配算法（buddy / free list / bump）不在此模块：属于 Allocator。
//! Core 职责：先验证（validate）再提交（commit）—— frame_db 体现两遍写入。

// TODO(人类实现者): 帧真相分配器（bump allocator）实现于 alloc.rs —— 起点从
// `__bootstrap_end` 之后拿，分配给帧数组本体，替代静态 BSS 帧池。

mod frame_db;

pub use frame_db::{init, FrameDatabase, merge_overlapping};

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

/// Core 的每帧真相记录（FrameDatabase 契约的一部分）。
/// 保留（人类实现者拍板）：未来加 generation/refcount 等字段。
/// 切片式下数组本体由调用方（如 lib.rs 帧池）持有并构造，故需对外可见。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameMeta {
    pub(crate) state: FrameState,
}

impl FrameMeta {
    pub(crate) const fn new(state: FrameState) -> Self {
        Self { state }
    }
}

// ④ errors
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameError {
    OutOfRange,
    NotFree,
    AlreadyOwned,
    Permission,
}
