//! 每实例状态 + 构造期分配 / 清理（virtio_blk 私有）。
//!
//! 报告数据是**每实例**的：`NoMatch` / `Match` 结果只属于本实例，不会被后续实例
//! 覆盖；`attached` 区分 report-only 实例（它绝不碰全局设备状态，见 `lib.rs` 的
//! destroy 钩子）。
//!
//! backing 经 [`kcomp_sdk::mem`] 向 Core 取（`kcore_memory_acquire`）：`region`
//! 字段保存 acquire 交付的**原样 view**，构造期失败清理凭它原样交回——Core 无
//! 账本，view 自身就是身份。

use kcomp_sdk::abi::MemoryView;
use kcomp_sdk::errno::Errno;
use kcomp_sdk::mem;
use kcomp_sdk::probe::{KCOMP_DRIVER_CREATE_NAME_MAX, ProbeReply};

/// 每实例状态：`kcomp_instance_create` 分配、写回 `out_state`，由结果 Server Task 借用，构造期复制端口名。
#[repr(C)]
pub(crate) struct VirtioBlkState {
    /// 本实例是否真的 attach 了设备。report-only 实例恒 `false`——它的 destroy
    /// 绝不碰全局设备状态（否则会复位已 attach 实例的设备）。
    pub(crate) attached: bool,
    /// `probe.result` 的 8 字节回复（outcome i32 LE + detail u32 LE）。
    pub(crate) result: ProbeReply,
    pub(crate) name: [u8; KCOMP_DRIVER_CREATE_NAME_MAX],
    pub(crate) name_len: usize,
    /// 本 state 的 backing 窗口（acquire 交付，构造期失败清理时原样交回）。
    pub(crate) region: MemoryView,
}

/// 构造期分配每实例 state（失败返回 NULL）。
pub(crate) fn alloc_state(name: &[u8]) -> *mut VirtioBlkState {
    let view = match mem::mem_alloc(
        core::mem::size_of::<VirtioBlkState>() as u64,
        core::mem::align_of::<VirtioBlkState>() as u64,
    ) {
        Ok(view) => view,
        Err(_) => return core::ptr::null_mut(),
    };
    let state = view.base as *mut VirtioBlkState;
    // SAFETY: state 是 acquire 交付、对齐满足的 VirtioBlkState 存储；ptr::write
    // 直接放置初始值（不读旧值）。默认结果 = 创建失败——只有在两个 endpoint 都
    // 发布成功之后才改写为 Match / NoMatch。
    unsafe {
        core::ptr::write(
            state,
            VirtioBlkState {
                attached: false,
                result: ProbeReply::creation_failed(Errno::EIO),
                name: [0; KCOMP_DRIVER_CREATE_NAME_MAX],
                name_len: name.len(),
                region: view,
            },
        );
    }
    // create config is borrowed only during create; the Task needs its own copy.
    unsafe { (&mut (*state).name)[..name.len()].copy_from_slice(name) };
    state
}

/// 构造期失败清理：state 尚未交给 Core（out_state 未写）且未发布给任何 consumer，
/// 按契约 §3「构造期清理由组件自己负责」归还 backing。
///
/// # Safety
/// `state` 必须来自本文件成功的一次 [`alloc_state`]，且未被写进 `out_state` /
/// 未被任何 endpoint publication 引用。
pub(crate) unsafe fn free_state(state: *mut VirtioBlkState) {
    // SAFETY: 调用者保证指针来自一次成功的 alloc_state；region 是 acquire 交付的
    // 原样 view，且该 backing 尚未交给 Core。
    let view = unsafe { (*state).region };
    let _ = mem::mem_release(view);
}
