//! 每实例状态 + 构造期分配 / 清理（virtio_blk 私有）。
//!
//! 报告数据是**每实例**的：`NoMatch` / `Match` 结果只属于本实例，不会被后续实例
//! 覆盖；`attached` 区分 report-only 实例（它绝不碰全局设备状态，见 `lib.rs` 的
//! destroy 钩子）。

use kcomp_sdk::abi;
use kcomp_sdk::errno::Errno;
use kcomp_sdk::probe::ProbeReply;

/// 每实例状态：`kcomp_instance_create` 分配、写回 `out_state`，经
/// `kcomp_service_dispatch` 的 `instance_state` 回放。
#[repr(C)]
pub(crate) struct VirtioBlkState {
    /// 本实例是否真的 attach 了设备。report-only 实例恒 `false`——它的 destroy
    /// 绝不碰全局设备状态（否则会复位已 attach 实例的设备）。
    pub(crate) attached: bool,
    /// `probe.result` 的 8 字节回复（outcome i32 LE + detail u32 LE）。
    pub(crate) result: [u8; ProbeReply::ENCODED_LEN],
}

/// 构造期分配每实例 state（失败返回 NULL）。
pub(crate) fn alloc_state() -> *mut VirtioBlkState {
    // SAFETY: 纯分配调用，无所有权语义；成功 = 对齐的 size 字节，失败 = NULL。
    let raw = unsafe {
        abi::kcore_heap_alloc(
            core::mem::size_of::<VirtioBlkState>(),
            core::mem::align_of::<VirtioBlkState>(),
        )
    };
    if raw.is_null() {
        return core::ptr::null_mut();
    }
    let state = raw.cast::<VirtioBlkState>();
    // SAFETY: 刚分配、无别名；ptr::write 直接放置初始值（不读旧值）。默认结果 =
    // 创建失败——只有在两个 endpoint 都发布成功之后才改写为 Match / NoMatch。
    unsafe {
        core::ptr::write(
            state,
            VirtioBlkState {
                attached: false,
                result: ProbeReply::creation_failed(Errno::EIO).encode(),
            },
        );
    }
    state
}

/// 构造期失败清理：state 尚未交给 Core（out_state 未写）且未发布给任何 consumer，
/// 按契约 §3「构造期清理由组件自己负责」归还。
///
/// # Safety
/// `state` 必须来自本文件成功的一次 [`alloc_state`]，且未被写进 `out_state` /
/// 未被任何 endpoint publication 引用。
pub(crate) unsafe fn free_state(state: *mut VirtioBlkState) {
    // SAFETY: 调用者保证指针来自一次成功的 alloc，size / align 完全一致。
    unsafe {
        let _ = abi::kcore_heap_dealloc(
            state.cast::<u8>(),
            core::mem::size_of::<VirtioBlkState>(),
            core::mem::align_of::<VirtioBlkState>(),
        );
    }
}
